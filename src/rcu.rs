use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
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
/// * `next` - Forms a linked list of all thread data structures.
/// * `local_counter` - Tracks the thread's local view of the global counter.
///
/// # Implementation Notes
///
/// The structure is allocated once per thread and lives for the entire thread lifetime.
/// It is managed through thread-local storage and a global linked list for writer
/// threads to traverse when checking for quiescent states.
struct ThreadData {
    /// Pointer to the next ThreadData in the global list.
    next: AtomicPtr<ThreadData>,
    /// Tracks the thread's local view of the global counter.
    local_counter: AtomicUsize,
}

impl ThreadData {
    /// Creates a new ThreadData instance with default values.
    fn new() -> Self {
        ThreadData {
            next: AtomicPtr::new(ptr::null_mut()),
            local_counter: AtomicUsize::new(0),
        }
    }
}

impl Drop for ThreadData {
    fn drop(&mut self) {
        unregister_thread(self);
    }
}

/// Registers a thread to participate in RCU operations by adding its ThreadData
/// to the global thread list.
///
/// This function is automatically called when a thread first accesses its thread-local
/// RCU data structure. It ensures that writer threads can find all reader threads
/// when checking for quiescent states.
///
/// # Safety
///
/// This function is safe to call multiple times but will only register a thread once.
/// The ThreadData structure must have static lifetime as it will be accessed by other threads.
fn register_thread(td: &ThreadData) {
    let td_ptr = td as *const _ as *mut _;
    loop {
        let head = THREAD_LIST_HEAD.load(Ordering::Acquire);
        (*td).next.store(head, Ordering::Relaxed);
        if THREAD_LIST_HEAD
            .compare_exchange(head, td_ptr, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            break;
        }
    }
}

/// Unregisters a thread from participating in RCU operations by removing its ThreadData
/// from the global thread list.
///
/// This function is called when a thread's ThreadData is dropped.
fn unregister_thread(td: &ThreadData) {
    let td_ptr = td as *const _ as *mut ThreadData;
    loop {
        let mut prev_ptr = &THREAD_LIST_HEAD as *const _ as *mut AtomicPtr<ThreadData>;
        let mut current_ptr = THREAD_LIST_HEAD.load(Ordering::Acquire);

        while !current_ptr.is_null() {
            if current_ptr == td_ptr {
                let next = (*td).next.load(Ordering::Acquire);
                if unsafe {
                    (*prev_ptr)
                        .compare_exchange(current_ptr, next, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                } {
                    return;
                } else {
                    break;
                }
            }
            prev_ptr = unsafe { &(*current_ptr).next as *const _ as *mut AtomicPtr<ThreadData> };
            current_ptr = unsafe { (*current_ptr).next.load(Ordering::Acquire) };
        }

        thread::yield_now();
    }
}

static GLOBAL_COUNTER: AtomicUsize = AtomicUsize::new(1);
static THREAD_LIST_HEAD: AtomicPtr<ThreadData> = AtomicPtr::new(ptr::null_mut());

thread_local! {
    /// Thread-local storage for each thread's ThreadData.
    static THREAD_DATA: Box<ThreadData> = {
        // Allocate and leak a new ThreadData instance to ensure it lives for the thread's lifetime.
        let td = Box::new(ThreadData::new());
        // Register the thread's ThreadData in the global thread list.
        register_thread(&td);
        td
    };
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

    /// Marks the beginning of an RCU read-side critical section.
    ///
    /// This function initializes the thread's `local_counter` with the current `GLOBAL_COUNTER`.
    /// It should be called before accessing RCU-protected data.
    fn rcu_read_lock(&self) {
        THREAD_DATA.with(|td| {
            td.local_counter
                .store(GLOBAL_COUNTER.load(Ordering::SeqCst), Ordering::SeqCst);
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
    /// * Increments the global counter atomically.
    /// * Checks each registered thread's local counter.
    /// * A thread has passed through a quiescent state if its local counter has caught up.
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
        let latest_counter = GLOBAL_COUNTER.fetch_add(1, Ordering::AcqRel);
        let old_counter = latest_counter - 1;

        loop {
            let mut ptr = THREAD_LIST_HEAD.load(Ordering::SeqCst);
            let mut all_threads_observed = true;

            unsafe {
                while !ptr.is_null() {
                    let td = &*ptr;
                    let local_counter = td.local_counter.load(Ordering::SeqCst);
                    if local_counter < old_counter {
                        println!("Thread {} not observed", ptr as usize);
                        all_threads_observed = false;
                        break;
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

impl<T> Drop for Rcu<T> {
    fn drop(&mut self) {
        // Ensure all updates are visible and process callbacks.
        self.call_rcu();

        // Free the current data if it exists.
        let ptr = self.ptr.swap(ptr::null_mut(), Ordering::SeqCst);
        if !ptr.is_null() {
            unsafe {
                drop(Box::from_raw(ptr));
            }
        }
    }
}

/// Default callback for freeing old data.
fn free_callback<T>(ptr: *mut T) {
    unsafe {
        drop(Box::from_raw(ptr));
    }
}
