use read_copy_update::{call_rcu, synchronize_rcu, Rcu};
use std::sync::Arc;
use std::thread;

/// Increment the value stored in the RCU instance.
/// Uses `try_update` to safely perform the update.
fn increment_rcu_value(rcu: &Rcu<i32>) {
    rcu.try_update(|current| current + 1).unwrap();
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
    // Create a new RCU instance wrapped in Arc for shared ownership across threads.
    let rcu = Arc::new(Rcu::new(42));

    // Define the number of threads and the number of increments each thread will perform.
    let num_threads = 10;
    let increments_per_thread = 100;
    let read_iterations = 1000;

    let mut handles = Vec::new();

    // Spawn multiple threads to perform concurrent increments.
    for _ in 0..num_threads {
        let rcu_clone = Arc::clone(&rcu);
        let handle = thread::spawn(move || {
            for _ in 0..increments_per_thread {
                increment_rcu_value(&rcu_clone);
            }
        });
        handles.push(handle);
    }

    // Spawn multiple threads to perform concurrent reads.
    for _ in 0..num_threads {
        let rcu_clone = Arc::clone(&rcu);
        let handle = thread::spawn(move || {
            simulate_read(&rcu_clone, read_iterations);
        });
        handles.push(handle);
    }

    // Wait for all threads to finish.
    for handle in handles {
        handle.join().unwrap();
    }

    // Ensure all updates are visible and process callbacks.
    synchronize_rcu();
    call_rcu(&rcu);

    // Read the final value from the RCU-protected data.
    let final_value = rcu.read(|d| *d).unwrap();
    println!("Final value: {}", final_value);
    // Verify that the final value matches the expected result.
    assert_eq!(final_value, 42 + num_threads * increments_per_thread);

    println!("Test passed. Final value: {}", final_value);
}

#[test]
fn test_rcu_callback_processing() {
    // Create a new RCU instance wrapped in Arc for shared ownership across threads.
    let rcu = Arc::new(Rcu::new(42));

    for _ in 0..10 {
        increment_rcu_value(&rcu);
    }

    synchronize_rcu();
    call_rcu(&rcu);

    rcu.read(|val| {
        println!("Value after callback processing: {}", val);
        assert!(*val >= 42);
    })
    .unwrap();
}
