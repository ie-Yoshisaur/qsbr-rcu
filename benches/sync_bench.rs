use criterion::{criterion_group, criterion_main, Criterion};
use read_copy_update::{rcu_thread_spawn, Rcu};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

use rand::Rng;

const NUM_THREADS: usize = 8;
const NUM_OPERATIONS: usize = 100_000;
fn bench_1_write_99_read(c: &mut Criterion) {
    c.bench_function("Mutex - 1% Write / 99% Read", |b| {
        b.iter(|| {
            let mutex = Arc::new(Mutex::new(0));
            let mut handles = Vec::new();

            for _ in 0..NUM_THREADS {
                let mutex_clone = Arc::clone(&mutex);
                let handle = thread::spawn(move || {
                    let mut rng = rand::thread_rng();
                    for _ in 0..NUM_OPERATIONS {
                        let op: usize = rng.gen_range(0..100);
                        if op < 1 {
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

    c.bench_function("RwLock - 1% Write / 99% Read", |b| {
        b.iter(|| {
            let rwlock = Arc::new(RwLock::new(0));
            let mut handles = Vec::new();

            for _ in 0..NUM_THREADS {
                let rwlock_clone = Arc::clone(&rwlock);
                let handle = thread::spawn(move || {
                    let mut rng = rand::thread_rng();
                    for _ in 0..NUM_OPERATIONS {
                        let op: usize = rng.gen_range(0..100);
                        if op < 1 {
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

    c.bench_function("RCU - 1% Write / 99% Read", |b| {
        b.iter(|| {
            let rcu = Arc::new(Rcu::new(0));
            let mut handles = Vec::new();

            for _ in 0..NUM_THREADS {
                let rcu_clone = Arc::clone(&rcu);
                let handle = rcu_thread_spawn!({
                    let mut rng = rand::thread_rng();
                    for _ in 0..NUM_OPERATIONS {
                        let op: usize = rng.gen_range(0..100);
                        if op < 1 {
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
}

fn bench_10_write_90_read(c: &mut Criterion) {
    c.bench_function("Mutex - 10% Write / 90% Read", |b| {
        b.iter(|| {
            let mutex = Arc::new(Mutex::new(0));
            let mut handles = Vec::new();

            for _ in 0..NUM_THREADS {
                let mutex_clone = Arc::clone(&mutex);
                let handle = thread::spawn(move || {
                    let mut rng = rand::thread_rng();
                    for _ in 0..NUM_OPERATIONS {
                        let op: usize = rng.gen_range(0..100);
                        if op < 10 {
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

    c.bench_function("RwLock - 10% Write / 90% Read", |b| {
        b.iter(|| {
            let rwlock = Arc::new(RwLock::new(0));
            let mut handles = Vec::new();

            for _ in 0..NUM_THREADS {
                let rwlock_clone = Arc::clone(&rwlock);
                let handle = thread::spawn(move || {
                    let mut rng = rand::thread_rng();
                    for _ in 0..NUM_OPERATIONS {
                        let op: usize = rng.gen_range(0..100);
                        if op < 10 {
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

    c.bench_function("RCU - 10% Write / 90% Read", |b| {
        b.iter(|| {
            let rcu = Arc::new(Rcu::new(0));
            let mut handles = Vec::new();

            for _ in 0..NUM_THREADS {
                let rcu_clone = Arc::clone(&rcu);
                let handle = rcu_thread_spawn!({
                    let mut rng = rand::thread_rng();
                    for _ in 0..NUM_OPERATIONS {
                        let op: usize = rng.gen_range(0..100);
                        if op < 10 {
                            // 書き込み
                            rcu_clone
                                .try_update(|val| val + 1)
                                .expect("RCU update failed");
                        } else {
                            // 読み取り
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
}

fn bench_50_write_50_read(c: &mut Criterion) {
    c.bench_function("Mutex - 50% Write / 50% Read", |b| {
        b.iter(|| {
            let mutex = Arc::new(Mutex::new(0));
            let mut handles = Vec::new();

            for _ in 0..NUM_THREADS {
                let mutex_clone = Arc::clone(&mutex);
                let handle = thread::spawn(move || {
                    let mut rng = rand::thread_rng();
                    for _ in 0..NUM_OPERATIONS {
                        let op: usize = rng.gen_range(0..100);
                        if op < 50 {
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

    c.bench_function("RwLock - 50% Write / 50% Read", |b| {
        b.iter(|| {
            let rwlock = Arc::new(RwLock::new(0));
            let mut handles = Vec::new();

            for _ in 0..NUM_THREADS {
                let rwlock_clone = Arc::clone(&rwlock);
                let handle = thread::spawn(move || {
                    let mut rng = rand::thread_rng();
                    for _ in 0..NUM_OPERATIONS {
                        let op: usize = rng.gen_range(0..100);
                        if op < 50 {
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

    c.bench_function("RCU - 50% Write / 50% Read", |b| {
        b.iter(|| {
            let rcu = Arc::new(Rcu::new(0));
            let mut handles = Vec::new();

            for _ in 0..NUM_THREADS {
                let rcu_clone = Arc::clone(&rcu);
                let handle = rcu_thread_spawn!({
                    let mut rng = rand::thread_rng();
                    for _ in 0..NUM_OPERATIONS {
                        let op: usize = rng.gen_range(0..100);
                        if op < 50 {
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
}

criterion_group!(
    benches,
    bench_1_write_99_read,
    bench_10_write_90_read,
    bench_50_write_50_read
);
criterion_main!(benches);
