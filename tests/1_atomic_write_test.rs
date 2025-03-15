use qsbr_rcu::{rcu_thread_spawn, Rcu};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Increment the value stored in the RCU instance.
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
fn atomic_write_test() {
    let rcu = Arc::new(Rcu::new(42));
    let num_threads = 10;
    let increments_per_thread = 100;
    let read_iterations = 1000;
    let mut handles: Vec<JoinHandle<()>> = Vec::new();

    // Update threads
    for _ in 0..num_threads {
        let rcu_clone = Arc::clone(&rcu);
        let handle = rcu_thread_spawn(rcu_clone.clone(), move || {
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
        let handle = rcu_thread_spawn(rcu_clone.clone(), move || {
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
