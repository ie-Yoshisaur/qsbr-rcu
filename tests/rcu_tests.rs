use read_copy_update::Rcu;
use std::thread;

// マクロを使用するためにクレート全体をインポート
use read_copy_update::define_rcu;

// Define a static RCU instance for i32 with an initial value of 42.
define_rcu!(RCU_INT, get_rcu_int, i32, 42);

/// Increments the value stored in the RCU-protected i32 by 1.
/// Uses `try_update` to safely perform the update.
fn increment_rcu_value(rcu: &Rcu<i32>) {
    rcu.try_update(|current| current + 1).unwrap();
}

#[test]
fn test_rcu_increment() {
    let rcu = get_rcu_int();

    // Define the number of threads and the number of increments each thread will perform.
    let num_threads = 10;
    let increments_per_thread = 1000;

    let mut handles = Vec::new();

    // Spawn multiple threads to perform concurrent increments.
    for _ in 0..num_threads {
        let rcu_ref = rcu;
        let handle = thread::spawn(move || {
            for _ in 0..increments_per_thread {
                increment_rcu_value(rcu_ref);
            }
        });
        handles.push(handle);
    }

    // Wait for all threads to finish execution.
    for handle in handles {
        handle.join().unwrap();
    }

    // Read the final value from the RCU-protected data.
    let final_value = rcu.read(|d| *d).unwrap();
    println!("Final value: {}", final_value);
    // Verify that the final value matches the expected result.
    assert_eq!(final_value, 42 + num_threads * increments_per_thread);
}
