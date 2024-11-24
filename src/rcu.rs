use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::thread;

/// Represents a callback structure used for deferred cleanup in RCU.
/// Each callback contains a function pointer (`func`) and associated data (`data`) to be processed later.
///
/// `Callback` forms a linked list through the `next` field, enabling multiple callbacks to be queued.
struct Callback<T> {
    func: fn(*mut T),             // Function to process or free the data.
    data: *mut T,                 // Pointer to the associated data.
    next: AtomicPtr<Callback<T>>, // Pointer to the next callback in the queue.
}

/// Thread-specific data structure used to track the state of each thread in RCU operations.
///
/// This structure is key to the QSBR (Quiescent State Based RCU) implementation. Each thread
/// maintains its own counter that is compared against the global counter to determine if the
/// thread has passed through a quiescent state since a particular write operation.
///
/// # Fields
///
/// * `next` - Forms a linked list of all thread data structures
/// * `registered` - Indicates if this thread is participating in RCU operations
/// * `local_counter` - Tracks the thread's local view of the global counter
///
/// # Implementation Notes
///
/// The structure is allocated once per thread and lives for the entire thread lifetime.
/// It is managed through thread-local storage and a global linked list for writer
/// threads to traverse when checking for quiescent states.
struct ThreadData {
    /// Pointer to the next ThreadData in the global list.
    next: AtomicPtr<ThreadData>,
    /// Indicates whether the thread has been registered for RCU operations.
    registered: AtomicBool,
    /// Indicates whether the thread is within an RCU read-side critical section.
    local_counter: AtomicUsize,
}

impl ThreadData {
    /// Creates a new ThreadData instance with default values.
    fn new() -> Self {
        ThreadData {
            next: AtomicPtr::new(ptr::null_mut()),
            registered: AtomicBool::new(false),
            local_counter: AtomicUsize::new(0),
        }
    }
}

thread_local! {
    /// Thread-local storage for each thread's ThreadData.
    static THREAD_DATA: ThreadData =  ThreadData::new();
}

/// A lock-free synchronization structure based on Read-Copy-Update (RCU).
///
/// RCU allows readers to access shared data without blocking, while writers perform updates by
/// creating new versions of the data and replacing the old pointer atomically. Readers are guaranteed
/// to see either the old or the new version of the data.
///
/// This implementation also supports deferred cleanup of old data through callbacks.
///
/// # Examples
///
/// ```rust
/// use std::thread;
/// use std::sync::Arc;
/// use read_copy_update::Rcu;
///
/// // Create a new RCU instance wrapped in Arc for shared ownership across threads.
/// let rcu = Arc::new(Rcu::new(42));
///
/// // Reader thread accesses the current value.
/// let rcu_clone = Arc::clone(&rcu);
/// let reader = thread::spawn(move || {
///     rcu_clone.read(|val| {
///         println!("Read value: {}", val);
///     }).expect("Failed to read value");
/// });
///
/// // Updater thread increments the value.
/// let rcu_clone = Arc::clone(&rcu);
/// let updater = thread::spawn(move || {
///     rcu_clone.try_update(|val| val + 1).expect("Update failed");
///     println!("Value incremented.");
/// });
///
/// reader.join().unwrap();
/// updater.join().unwrap();
/// ```
pub struct Rcu<T> {
    ptr: AtomicPtr<T>, // Atomic pointer to the currently accessible data.
    callbacks: AtomicPtr<Callback<T>>, // Atomic pointer to the callback list for deferred cleanup.
    global_counter: AtomicUsize, // Global counter for synchronization
    thread_list_head: AtomicPtr<ThreadData>, // Head of the global thread list
}

impl<T> Rcu<T> {
    /// Creates a new RCU instance by allocating the provided data on the heap.
    ///
    /// This method ensures that the data is managed in a way compatible with RCU's requirements.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use read_copy_update::Rcu;
    ///
    /// let rcu = Rcu::new(42); // Initialize RCU with the value 42.
    /// ```
    pub fn new(data: T) -> Self {
        let boxed = Box::new(data);
        Rcu {
            ptr: AtomicPtr::new(Box::into_raw(boxed)),
            callbacks: AtomicPtr::new(ptr::null_mut()),
            global_counter: AtomicUsize::new(1),
            thread_list_head: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Reads the data protected by RCU in a read-side critical section.
    ///
    /// The provided closure is executed with a reference to the current data. If the pointer is null,
    /// an error is returned. This ensures safe access to RCU-protected data.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use read_copy_update::Rcu;
    ///
    /// let rcu = Rcu::new(42);
    /// rcu.read(|val| println!("Value: {}", val)).unwrap();
    /// ```
    pub fn read<F, R>(&self, f: F) -> Result<R, ()>
    where
        F: FnOnce(&T) -> R,
    {
        self.rcu_read_lock();
        let ptr = self.rcu_dereference();
        let result = unsafe {
            if ptr.is_null() {
                self.rcu_read_unlock();
                return Err(());
            }
            f(&*ptr)
        };
        self.rcu_read_unlock();
        Ok(result)
    }

    /// Updates the RCU-protected data using the provided closure.
    ///
    /// This method atomically replaces the current data with new data generated by the closure. The
    /// old data is safely reclaimed using deferred cleanup.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use read_copy_update::Rcu;
    ///
    /// let rcu = Rcu::new(42);
    /// rcu.try_update(|val| val + 1).unwrap();
    /// ```
    pub fn try_update<F>(&self, f: F) -> Result<(), ()>
    where
        F: Fn(&T) -> T,
    {
        loop {
            let old_ptr = self.ptr.load(Ordering::SeqCst);
            if old_ptr.is_null() {
                return Err(());
            }
            let new_data = unsafe { f(&*old_ptr) };
            let new_box = Box::new(new_data);
            let new_ptr = Box::into_raw(new_box);

            match self
                .ptr
                .compare_exchange(old_ptr, new_ptr, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => {
                    self.add_callback(free_callback, old_ptr);
                    return Ok(());
                }
                Err(_) => unsafe { drop(Box::from_raw(new_ptr)) },
            }
        }
    }

    /// Registers a callback to reclaim the old data.
    fn add_callback(&self, func: fn(*mut T), data: *mut T) {
        let cb = Box::new(Callback {
            func,
            data,
            next: AtomicPtr::new(ptr::null_mut()),
        });
        let cb_ptr = Box::into_raw(cb);

        loop {
            let head = self.callbacks.load(Ordering::Acquire);
            unsafe {
                (*cb_ptr).next.store(head, Ordering::Relaxed);
            }
            if self
                .callbacks
                .compare_exchange(head, cb_ptr, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
    }

    /// Processes all pending callbacks and executes them.
    ///
    /// This function should be called periodically to reclaim memory or perform other cleanup tasks
    /// for outdated RCU-protected data.
    fn process_callbacks(&self) {
        let mut cb_ptr = self.callbacks.swap(ptr::null_mut(), Ordering::AcqRel);
        while !cb_ptr.is_null() {
            unsafe {
                let cb = Box::from_raw(cb_ptr);
                (cb.func)(cb.data);
                cb_ptr = cb.next.load(Ordering::Acquire);
            }
        }
    }

    /// Registers a thread to participate in RCU operations by adding its ThreadData
    /// to the global thread list.
    ///
    /// This function is automatically called when a thread first accesses its thread-local
    /// RCU data structure. It ensures that writer threads can find all reader threads
    /// when checking for quiescent states.
    ///
    /// # Implementation Details
    ///
    /// * Marks the thread as registered using atomic operations
    /// * Adds the thread's data to the global linked list using a lock-free push
    /// * Uses SeqCst ordering to ensure visibility across all threads
    ///
    /// # Safety
    ///
    /// This function is safe to call multiple times but will only register a thread once.
    /// The ThreadData structure must have static lifetime as it will be accessed by other threads.
    pub fn register_thread(&self) {
        THREAD_DATA.with(|td| {
            // Mark the thread as registered.
            td.registered.store(true, Ordering::SeqCst);
            let td_ptr = td as *const _ as *mut _;

            // Insert the new ThreadData at the head of the global list.
            td.next.store(
                self.thread_list_head.load(Ordering::SeqCst),
                Ordering::SeqCst,
            );
            self.thread_list_head.store(td_ptr, Ordering::SeqCst);
        });
    }

    /// Marks the beginning of an RCU read-side critical section.
    ///
    /// This function initializes the thread's `local_counter` with the current `global_counter`.
    /// It should be called before accessing RCU-protected data.
    fn rcu_read_lock(&self) {
        THREAD_DATA.with(|td| {
            let global_counter = self.global_counter.load(Ordering::SeqCst);
            td.local_counter.store(global_counter, Ordering::SeqCst);
            std::sync::atomic::fence(Ordering::SeqCst);
        });
    }

    /// Marks the end of an RCU read-side critical section.
    ///
    /// This function does nothing but is provided for symmetry with `rcu_read_lock`.
    fn rcu_read_unlock(&self) {}

    /// Waits until all ongoing RCU read-side critical sections have completed.
    ///
    /// # Implementation Details
    ///
    /// * Increments the global counter atomically
    /// * Checks each registered thread's local counter
    /// * A thread has passed through a quiescent state if its local counter has caught up
    ///
    /// # Examples
    ///
    /// ```rust
    /// use read_copy_update::Rcu;
    ///
    /// let rcu = Rcu::new(42);
    /// rcu.synchronize_rcu();
    /// ```
    pub fn synchronize_rcu(&self) {
        let latest_counter = self.global_counter.fetch_add(1, Ordering::AcqRel);
        let old_counter = latest_counter - 1;

        loop {
            let mut ptr = self.thread_list_head.load(Ordering::SeqCst);
            let mut all_threads_observed = true;

            unsafe {
                while !ptr.is_null() {
                    let td = &*ptr;
                    if td.registered.load(Ordering::SeqCst) {
                        let local_counter = td.local_counter.load(Ordering::SeqCst);
                        if local_counter < old_counter {
                            all_threads_observed = false;
                            break;
                        }
                    }
                    ptr = td.next.load(Ordering::SeqCst);
                }
            }

            if all_threads_observed {
                break;
            } else {
                thread::yield_now();
            }
        }
    }

    /// Processes all pending callbacks after ensuring all read-side critical sections have completed.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use read_copy_update::Rcu;
    ///
    /// let rcu = Rcu::new(42);
    /// rcu.try_update(|val| val + 1).unwrap();
    /// rcu.call_rcu();
    /// ```
    pub fn call_rcu(&self) {
        self.synchronize_rcu();
        self.process_callbacks();
    }

    /// Safely dereferences an RCU-protected atomic pointer.
    ///
    /// This function ensures that the read is consistent and follows proper
    /// memory ordering rules.
    fn rcu_dereference(&self) -> *mut T {
        let ptr = self.ptr.load(Ordering::SeqCst);
        std::sync::atomic::fence(Ordering::SeqCst);
        ptr
    }
}

/// Default callback for freeing old data.
fn free_callback<T>(ptr: *mut T) {
    unsafe {
        drop(Box::from_raw(ptr));
    }
}
