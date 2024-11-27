use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use read_copy_update::{rcu_thread_spawn, Rcu};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

use rand::Rng;

const NUM_THREADS: usize = 8;
const NUM_OPERATIONS: usize = 100_000;

fn benchmark(c: &mut Criterion) {
    let scenarios = vec![("1%", 1, 99), ("10%", 10, 90), ("50%", 50, 50)];

    for (scenario_name, write_pct, _read_pct) in scenarios {
        let mut group = c.benchmark_group(scenario_name);

        group.bench_with_input(BenchmarkId::new("A_Mutex", scenario_name), &0, |b, _| {
            b.iter(|| {
                let mutex = Arc::new(Mutex::new(0));
                let mut handles = Vec::new();

                for _ in 0..NUM_THREADS {
                    let mutex_clone = Arc::clone(&mutex);
                    let handle = thread::spawn(move || {
                        let mut rng = rand::thread_rng();
                        for _ in 0..NUM_OPERATIONS {
                            let op: usize = rng.gen_range(0..100);
                            if op < write_pct {
                                let mut data = mutex_clone.lock().unwrap();
                                *data += 1;
                            } else {
                                let data = mutex_clone.lock().unwrap();
                                let _ = *data;
                            }
                        }
                    });
                    handles.push(handle);
                }

                for handle in handles {
                    handle.join().unwrap();
                }
            });
        });

        group.bench_with_input(BenchmarkId::new("B_RwLock", scenario_name), &0, |b, _| {
            b.iter(|| {
                let rwlock = Arc::new(RwLock::new(0));
                let mut handles = Vec::new();

                for _ in 0..NUM_THREADS {
                    let rwlock_clone = Arc::clone(&rwlock);
                    let handle = thread::spawn(move || {
                        let mut rng = rand::thread_rng();
                        for _ in 0..NUM_OPERATIONS {
                            let op: usize = rng.gen_range(0..100);
                            if op < write_pct {
                                let mut data = rwlock_clone.write().unwrap();
                                *data += 1;
                            } else {
                                let data = rwlock_clone.read().unwrap();
                                let _ = *data;
                            }
                        }
                    });
                    handles.push(handle);
                }

                for handle in handles {
                    handle.join().unwrap();
                }
            });
        });

        group.bench_with_input(BenchmarkId::new("C_RCU", scenario_name), &0, |b, _| {
            b.iter(|| {
                let rcu = Arc::new(Rcu::new(0));
                let mut handles = Vec::new();

                for _ in 0..NUM_THREADS {
                    let rcu_clone = Arc::clone(&rcu);
                    let handle = rcu_thread_spawn!({
                        let mut rng = rand::thread_rng();
                        for _ in 0..NUM_OPERATIONS {
                            let op: usize = rng.gen_range(0..100);
                            if op < write_pct {
                                rcu_clone
                                    .try_update(|val| val + 1)
                                    .expect("RCU update failed");
                            } else {
                                rcu_clone
                                    .read(|val| {
                                        let _ = *val;
                                    })
                                    .expect("RCU read failed");
                            }
                        }
                    });
                    handles.push(handle);
                }

                for handle in handles {
                    handle.join().unwrap();
                }

                rcu.synchronize_rcu();
                rcu.process_callbacks();
            });
        });

        group.finish();
    }
}

criterion_group!(benches, benchmark);
criterion_main!(benches);
