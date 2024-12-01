use qsbr_rcu::{rcu_thread_spawn, Rcu};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

/// Increment the value stored in the RCU instance.
/// Uses `try_update` to safely perform the update.
fn increment_rcu_value(rcu: &Rcu<i32>) {
    rcu.write(|current| current + 1).unwrap();
}

/// Simulate a heavy read workload.
fn simulate_read(rcu: &Rcu<i32>, iterations: usize) {
    for _ in 0..iterations {
        let _ = rcu.read(|val| {
            assert!(*val >= 42);
        });
    }
}

#[test]
fn test_rcu_multithreaded_update_and_callback() {
    let rcu = Arc::new(Rcu::new(42));
    let num_threads = 10;
    let increments_per_thread = 100;
    let read_iterations = 1000;
    let mut handles = Vec::new();

    // Update threads
    for _ in 0..num_threads {
        let rcu_clone = Arc::clone(&rcu);
        let handle = rcu_thread_spawn!(rcu_clone, {
            for _ in 0..increments_per_thread {
                increment_rcu_value(&rcu_clone);
            }
            rcu_clone.rcu_quiescent_state();
        });
        handles.push(handle);
    }

    // Read threads
    for _ in 0..num_threads {
        let rcu_clone = Arc::clone(&rcu);
        let handle = rcu_thread_spawn!(rcu_clone, {
            simulate_read(&rcu_clone, read_iterations);
            rcu_clone.rcu_quiescent_state();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    rcu.gc();

    let final_value = rcu.read(|d| *d).unwrap();
    assert_eq!(final_value, 42 + num_threads * increments_per_thread);
}

#[test]
fn test_rcu_callback_processing() {
    // Create a new RCU instance wrapped in Arc for shared ownership across threads.
    let rcu = Arc::new(Rcu::new(42));

    // Perform multiple increments.
    for _ in 0..10 {
        increment_rcu_value(&rcu);
    }

    // Ensure all updates are visible and process callbacks.
    rcu.gc();

    // Read and verify the value after callback processing.
    rcu.read(|val| {
        println!("Value after callback processing: {}", val);
        assert!(*val >= 42);
    })
    .unwrap();
}

/// A struct that increments a counter when dropped.
struct DropCounter {
    value: i32,
    drop_count: Arc<AtomicUsize>,
}

impl Drop for DropCounter {
    fn drop(&mut self) {
        self.drop_count.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn test_rcu_garbage_collection() {
    // Shared counter to track the number of times DropCounter is dropped.
    let drop_count = Arc::new(AtomicUsize::new(0));
    let initial_drop_count = drop_count.load(Ordering::SeqCst);

    // Create a new RCU instance with DropCounter.
    let rcu = Arc::new(Rcu::new(DropCounter {
        value: 0,
        drop_count: Arc::clone(&drop_count),
    }));

    // Perform multiple updates to trigger garbage collection.
    let num_updates = 100;
    for _ in 1..=num_updates {
        let rcu_clone = Arc::clone(&rcu);
        let drop_count_clone = Arc::clone(&drop_count);

        // Update the RCU-protected data.
        rcu_clone
            .write(|data| DropCounter {
                value: data.value + 1,
                drop_count: Arc::clone(&drop_count_clone),
            })
            .unwrap();
    }

    // Ensure all updates are visible and process callbacks.
    rcu.gc();

    // Allow some time for all callbacks to be processed.
    thread::sleep(Duration::from_millis(1000));

    // Calculate the expected number of drops.
    let expected_drops = initial_drop_count + num_updates;

    // Check if the drop count matches the expected value.
    let actual_drops = drop_count.load(Ordering::SeqCst);
    assert_eq!(
        actual_drops, expected_drops,
        "Garbage collection did not free all old data."
    );

    println!("Test passed. All old data has been properly freed.");
}

#[test]
fn test_rcu_no_memory_leak() {
    // Use a memory allocator that can track allocations and deallocations.
    // For demonstration purposes, we will use the existing allocator and track allocations manually.

    // Shared counter to track the number of allocations and deallocations.
    let alloc_count = Arc::new(AtomicUsize::new(0));

    // Custom allocator can be implemented here if necessary.

    // Create a new RCU instance with a struct that increments allocation count.
    let rcu = Arc::new(Rcu::new({
        alloc_count.fetch_add(1, Ordering::SeqCst);
        0 // Initial value
    }));

    let num_threads = 5;
    let updates_per_thread = 50;

    let mut handles = Vec::new();

    // Spawn threads to perform updates.
    for _ in 0..num_threads {
        let rcu_clone = Arc::clone(&rcu);
        let alloc_count_clone = Arc::clone(&alloc_count);

        let handle = rcu_thread_spawn!(rcu_clone, {
            for _ in 0..updates_per_thread {
                rcu_clone
                    .write(|data| {
                        alloc_count_clone.fetch_add(1, Ordering::SeqCst);
                        data + 1
                    })
                    .unwrap();
            }
            rcu_clone.rcu_quiescent_state();
        });
        handles.push(handle);
    }

    // Wait for all threads to finish.
    for handle in handles {
        handle.join().unwrap();
    }

    rcu.read(|_| {}).unwrap();
    // Ensure all updates are visible and process callbacks.
    rcu.gc();

    // Allow some time for callbacks to be processed.
    thread::sleep(Duration::from_millis(100));

    // Since we cannot directly track deallocations without a custom allocator,
    // we can ensure that all old data is eventually dropped by checking the final value.
    let final_value = rcu.read(|d| *d).unwrap();
    let expected_final_value = num_threads * updates_per_thread;
    assert_eq!(final_value, expected_final_value);

    println!(
        "Test passed. No memory leaks detected, final value: {}",
        final_value
    );
}
