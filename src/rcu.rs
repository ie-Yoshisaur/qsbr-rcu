use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::thread;
use std::thread::ThreadId;

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
///     rcu_clone.write(|val| val + 1).expect("Update failed");
///     println!("Value incremented.");
/// });
///
/// reader.join().unwrap();
/// updater.join().unwrap();
/// ```
pub struct Rcu<T> {
    ptr: AtomicPtr<T>, // Atomic pointer to the currently accessible data.
    thread_list_head: AtomicPtr<ThreadData>,
    callbacks: AtomicPtr<Callback<T>>, // Atomic pointer to the callback list for deferred cleanup.
    global_counter: AtomicUsize,
    sync_lock: AtomicBool,
    callback_lock: AtomicBool,
    thread_list_lock: AtomicBool,
}

impl<T> Rcu<T> {
    /// Creates a new RCU instance by allocating the provided data on the heap.
    ///
    /// This method ensures that the data is managed in a way compatible with RCU's requirements.
    /// Additionally, it registers the current thread's `ThreadData` with the newly created RCU instance.
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
        let rcu = Rcu {
            ptr: AtomicPtr::new(Box::into_raw(boxed)),
            thread_list_head: AtomicPtr::new(ptr::null_mut()),
            callbacks: AtomicPtr::new(ptr::null_mut()),
            global_counter: AtomicUsize::new(1),
            sync_lock: AtomicBool::new(false),
            callback_lock: AtomicBool::new(false),
            thread_list_lock: AtomicBool::new(false),
        };

        // Register the current thread's ThreadData with this RCU instance.
        let td = Box::new(ThreadData::new(thread::current().id()));
        let td_ptr: *mut ThreadData = Box::into_raw(td);
        rcu.register_thread(unsafe { &*td_ptr });

        rcu
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
        // Register the thread with this RCU instance if not already registered.
        let td = self.get_or_register_thread();
        td.rcu_read_lock(&self.global_counter);
        let ptr = self.rcu_dereference();
        let result = unsafe {
            if ptr.is_null() {
                td.rcu_read_unlock();
                return Err(());
            }
            f(&*ptr)
        };
        td.rcu_read_unlock();
        Ok(result)
    }

    /// Safely dereferences an RCU-protected atomic pointer.
    ///
    /// This function ensures that the read is consistent and follows proper
    /// memory ordering rules.
    fn rcu_dereference(&self) -> *mut T {
        let ptr = self.ptr.load(Ordering::Acquire);
        std::sync::atomic::fence(Ordering::Acquire);
        ptr
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
    /// rcu.write(|val| val + 1).unwrap();
    /// ```
    pub fn write<F>(&self, f: F) -> Result<(), ()>
    where
        F: Fn(&T) -> T,
    {
        loop {
            let old_ptr = self.ptr.load(Ordering::Acquire);
            if old_ptr.is_null() {
                return Err(());
            }
            let new_data = unsafe { f(&*old_ptr) };
            let new_box = Box::new(new_data);
            let new_ptr = Box::into_raw(new_box);

            match self
                .ptr
                .compare_exchange(old_ptr, new_ptr, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    self.lock_callback();
                    self.add_callback(free_callback, old_ptr);
                    self.unlock_callback();
                    return Ok(());
                }
                Err(_) => unsafe { drop(Box::from_raw(new_ptr)) },
            }
        }
    }

    /// Registers a callback to reclaim the old data.
    ///
    /// This function queues a callback that will be executed once it's safe to reclaim the old data.
    ///
    /// # Parameters
    ///
    /// - `func`: The callback function to execute.
    /// - `data`: The data to be passed to the callback.
    fn add_callback(&self, func: fn(*mut T), data: *mut T) {
        let cb = Box::new(Callback {
            func,
            data,
            next: AtomicPtr::new(ptr::null_mut()),
        });
        let cb_ptr = Box::into_raw(cb);

        unsafe {
            let head = self.callbacks.load(Ordering::Acquire);
            (*cb_ptr).next.store(head, Ordering::Relaxed);
            self.callbacks.store(cb_ptr, Ordering::Release);
        }
    }

    /// Marks the current thread as having reached a quiescent state.
    ///
    /// This function should be called periodically by threads to indicate that
    /// they are in a quiescent state.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use read_copy_update::Rcu;
    ///
    /// let rcu = Rcu::new(42);
    /// rcu.rcu_quiescent_state(); // Indicate quiescent state
    /// ```
    pub fn rcu_quiescent_state(&self) {
        if let Some(td) = self.get_thread_data() {
            td.local_counter
                .store(self.global_counter.load(Ordering::SeqCst), Ordering::SeqCst);
            std::sync::atomic::fence(Ordering::SeqCst);
        }
    }

    /// Initiates garbage collection by processing safe callbacks and cleaning up thread data.
    ///
    /// This method should be called periodically by writer threads to reclaim memory or perform
    /// other cleanup tasks for outdated RCU-protected data.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use read_copy_update::Rcu;
    ///
    /// let rcu = Rcu::new(42);
    /// rcu.gc(); // Perform garbage collection
    /// ```
    pub fn gc(&self) {
        let safe_callbacks = self.synchronize_rcu();
        self.process_callbacks(safe_callbacks);
        self.clean_thread_list();
    }

    /// Processes all pending callbacks and executes them.
    ///
    /// This function should be called periodically to reclaim memory or perform other cleanup tasks
    /// for outdated RCU-protected data.
    fn process_callbacks(&self, callbacks: *mut Callback<T>) {
        let mut cb_ptr = callbacks;
        while !cb_ptr.is_null() {
            unsafe {
                let cb = Box::from_raw(cb_ptr);
                (cb.func)(cb.data);
                cb_ptr = cb.next.load(Ordering::Acquire);
            }
        }
    }

    /// Waits until all ongoing RCU read-side critical sections have completed.
    ///
    /// This function ensures that all reader threads have passed through a quiescent state
    /// since the last update, making it safe to reclaim outdated data.
    ///
    /// # Implementation Details
    ///
    /// - Increments the global counter atomically.
    /// - Checks each registered thread's local counter.
    /// - A thread has passed through a quiescent state if its local counter has caught up.
    fn synchronize_rcu(&self) -> *mut Callback<T> {
        self.lock_sync();
        self.rcu_quiescent_state();
        let latest_counter = self.global_counter.fetch_add(1, Ordering::Relaxed);
        let old_counter = latest_counter;

        loop {
            let mut all_threads_observed = true;

            unsafe {
                let mut ptr = self.thread_list_head.load(Ordering::Acquire);
                while !ptr.is_null() {
                    let td = &*ptr;
                    if !td.active.load(Ordering::Acquire) {
                        ptr = td.next.load(Ordering::Acquire);
                        continue;
                    }

                    let local_counter = td.local_counter.load(Ordering::Acquire);
                    if local_counter < old_counter {
                        all_threads_observed = false;
                        break;
                    }
                    ptr = td.next.load(Ordering::Acquire);
                }
            }

            if all_threads_observed {
                break;
            } else {
                thread::yield_now();
            }
        }

        self.lock_callback();
        let safe_callback_list = self.callbacks.swap(ptr::null_mut(), Ordering::AcqRel);
        self.unlock_callback();
        self.unlock_sync();
        safe_callback_list
    }

    /// Registers a thread to participate in RCU operations.
    ///
    /// This function should be called when a thread starts using RCU-protected data to ensure that
    /// it is tracked for quiescent state detection.
    ///
    /// # Parameters
    ///
    /// - `td`: A reference to the thread's `ThreadData` structure.
    ///
    pub fn register_thread(&self, td: &ThreadData) {
        let td_ptr: *mut ThreadData = td as *const _ as *mut ThreadData;

        td.active.store(true, Ordering::Release);
        let head = self.thread_list_head.load(Ordering::Acquire);
        td.next.store(head, Ordering::Relaxed);
        self.thread_list_head.store(td_ptr, Ordering::Release);
    }

    /// Unregisters a thread from participating in RCU operations.
    ///
    /// This function should be called when a thread no longer needs to participate in RCU operations,
    /// typically when the thread is about to exit.
    ///
    /// # Parameters
    ///
    /// - `td`: A reference to the thread's `ThreadData` structure.
    ///
    pub fn unregister_thread(&self, td: &ThreadData) {
        td.active.store(false, Ordering::Release);
    }

    /// Cleans up the thread list by removing inactive threads.
    ///
    /// This function iterates through the list of registered threads and removes any that are
    /// no longer active. It helps prevent memory leaks by ensuring that `ThreadData` structures
    /// for terminated threads are properly deallocated.
    fn clean_thread_list(&self) {
        self.lock_thread_list();
        let mut prev_ptr = &self.thread_list_head as *const _ as *mut AtomicPtr<ThreadData>;
        let mut current_ptr = self.thread_list_head.load(Ordering::Acquire);

        while !current_ptr.is_null() {
            let td = unsafe { &*current_ptr };
            if !td.active.load(Ordering::Acquire) {
                let next = td.next.load(Ordering::Acquire);
                unsafe {
                    (*prev_ptr).store(next, Ordering::Release);
                }
                unsafe {
                    let _ = Box::from_raw(current_ptr);
                }
                current_ptr = next;
            } else {
                prev_ptr =
                    unsafe { &(*current_ptr).next as *const _ as *mut AtomicPtr<ThreadData> };
                current_ptr = td.next.load(Ordering::Acquire);
            }
        }
        self.unlock_thread_list();
    }

    fn lock_sync(&self) {
        while self
            .sync_lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            thread::yield_now();
        }
    }

    fn unlock_sync(&self) {
        self.sync_lock.store(false, Ordering::Release);
    }

    fn lock_callback(&self) {
        while self
            .callback_lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            thread::yield_now();
        }
    }

    fn unlock_callback(&self) {
        self.callback_lock.store(false, Ordering::Release);
    }

    fn lock_thread_list(&self) {
        while self
            .thread_list_lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            thread::yield_now();
        }
    }

    fn unlock_thread_list(&self) {
        self.thread_list_lock.store(false, Ordering::Release);
    }

    /// Retrieves the current thread's `ThreadData` for this RCU instance.
    ///
    /// If the thread is not yet registered by this RCU instance, it registers a new `ThreadData`.
    fn get_or_register_thread(&self) -> &'static ThreadData {
        // Get the current thread's ID.
        let current_thread_id = thread::current().id();

        self.lock_thread_list();
        // Iterate through the thread list to find if this thread is already registered.
        let mut ptr = self.thread_list_head.load(Ordering::Acquire);
        while !ptr.is_null() {
            let td = unsafe { &*ptr };
            if td.thread_id == current_thread_id {
                self.unlock_thread_list();
                return td;
            }
            ptr = td.next.load(Ordering::Acquire);
        }

        // If not found, create and register a new ThreadData.
        let new_td = Box::new(ThreadData::new(thread::current().id()));
        let new_td_ptr: *mut ThreadData = Box::into_raw(new_td);
        self.register_thread(unsafe { &*new_td_ptr });
        self.unlock_thread_list();
        unsafe { &*new_td_ptr }
    }

    /// Attempts to retrieve the current thread's `ThreadData` for this RCU instance.
    ///
    /// Returns `Some(&ThreadData)` if found, or `None` otherwise.
    fn get_thread_data(&self) -> Option<&ThreadData> {
        let current_thread_id = thread::current().id();
        let mut ptr = self.thread_list_head.load(Ordering::Acquire);
        while !ptr.is_null() {
            let td = unsafe { &*ptr };
            if td.thread_id == current_thread_id {
                return Some(td);
            }
            ptr = td.next.load(Ordering::Acquire);
        }
        None
    }
}

impl<T> Drop for Rcu<T> {
    fn drop(&mut self) {
        self.gc();

        let ptr = self.ptr.swap(ptr::null_mut(), Ordering::SeqCst);
        if !ptr.is_null() {
            unsafe {
                drop(Box::from_raw(ptr));
            }
        }

        // Clean up remaining ThreadData
        self.clean_thread_list();

        // Process any remaining callbacks
        let remaining_callbacks = self.callbacks.swap(ptr::null_mut(), Ordering::AcqRel);
        self.process_callbacks(remaining_callbacks);
    }
}

/// Represents a callback structure used for deferred cleanup in RCU.
///
/// Each callback contains a function pointer (`func`) and associated data (`data`) to be processed later.
///
/// `Callback` forms a linked list through the `next` field, enabling multiple callbacks to be queued.
///
/// # Fields
///
/// * `func` - Function to process or free the data.
/// * `data` - Pointer to the associated data.
/// * `next` - Pointer to the next callback in the queue.
struct Callback<T> {
    func: fn(*mut T),             // Function to process or free the data.
    data: *mut T,                 // Pointer to the associated data.
    next: AtomicPtr<Callback<T>>, // Pointer to the next callback in the queue.
}

/// Default callback for freeing old data.
///
/// This function safely deallocates memory by converting the raw pointer back into a `Box` and
/// dropping it.
///
/// # Parameters
///
/// - `ptr`: A raw pointer to the data to be freed.
///
/// # Safety
///
/// - The pointer `ptr` must have been allocated using `Box::into_raw` and must not be used
///   after this function is called.
///
/// # Examples
///
/// ```rust
/// fn free_callback<T>(ptr: *mut T) {
///     unsafe {
///         drop(Box::from_raw(ptr));
///     }
/// }
/// ```
fn free_callback<T>(ptr: *mut T) {
    unsafe {
        drop(Box::from_raw(ptr));
    }
}

/// Represents a thread-specific data structure used to track the state of each thread in RCU operations.
///
/// This structure is key to the implementation of RCU. Each thread maintains its own `ThreadData`
/// that tracks its participation in read-side critical sections.
///
/// # Fields
///
/// * `next` - Forms a linked list of all thread data structures.
/// * `local_counter` - Tracks the thread's local view of the global counter.
/// * `active` - Indicates whether the thread is currently active in RCU operations.
/// * `thread_id` - The unique identifier of the thread.
pub struct ThreadData {
    /// Pointer to the next `ThreadData` in the global list.
    pub next: AtomicPtr<ThreadData>,
    /// Tracks the thread's local view of the global counter.
    pub local_counter: AtomicUsize,
    /// Indicates whether the thread is active or not.
    pub active: AtomicBool,
    /// The unique identifier of the thread.
    pub thread_id: ThreadId,
}

impl ThreadData {
    /// Creates a new `ThreadData` instance with the given thread ID.
    ///
    /// Initializes the `next` pointer to `null`, sets the `local_counter` to `0`,
    /// and marks the thread as inactive.
    pub fn new(thread_id: ThreadId) -> Self {
        ThreadData {
            next: AtomicPtr::new(ptr::null_mut()),
            local_counter: AtomicUsize::new(0),
            active: AtomicBool::new(false),
            thread_id,
        }
    }

    /// Marks the beginning of a read-side critical section.
    fn rcu_read_lock(&self, global_counter: &AtomicUsize) {
        self.local_counter
            .store(global_counter.load(Ordering::Relaxed), Ordering::SeqCst);
        std::sync::atomic::fence(Ordering::Acquire);
    }

    /// Marks the end of a read-side critical section.
    fn rcu_read_unlock(&self) {}
}

/// Marks the current thread as no longer participating in RCU operations.
///
/// This function should be called when a thread is about to terminate or when it no longer
/// needs to participate in RCU operations. It updates the thread's `active` status accordingly.
///
/// Since we're removing thread-local storage, this function requires the user to manually
/// manage `ThreadData` instances.
///
/// # Safety
///
/// - The `ThreadData` reference must belong to this thread and must have been registered
///   with the RCU instance.
///
/// # Examples
///
/// ```rust
/// use read_copy_update::{Rcu, drop_thread_data};
///
/// let rcu = Rcu::new(42);
/// // Assuming `td` is a reference to this thread's ThreadData for `rcu`.
/// // rcu.unregister_thread(&td);
/// ```
pub fn drop_thread_data(_rcu: &Rcu<()>, _td: &ThreadData) {
    // With the removal of thread-local storage, dropping thread data requires explicit
    // management by the user. This function is a placeholder.
    // Users should call `rcu.unregister_thread(&td)` before thread termination.
}

/// A macro for spawning a new thread that automatically registers and unregisters its
/// `ThreadData` with the provided `Rcu` instance.
///
/// This macro ensures that the thread participates in RCU operations by registering its
/// `ThreadData` upon creation and unregistering it before termination.
///
/// # Examples
///
/// ```rust
/// use std::sync::Arc;
/// use std::thread;
/// use read_copy_update::{rcu_thread_spawn, Rcu};
///
/// let rcu = Arc::new(Rcu::new(100));
///
/// let rcu_clone = Arc::clone(&rcu);
/// let handle = rcu_thread_spawn!(rcu_clone, {
///     // Thread body
///     rcu_clone.read(|val| {
///         println!("Thread read value: {}", val);
///     }).unwrap();
/// });
///
/// handle.join().unwrap();
/// ```
#[macro_export]
macro_rules! rcu_thread_spawn {
    ($rcu_clone:expr, $($body:tt)*) => {
        std::thread::spawn(move || {
            // Create and register ThreadData for this RCU instance and thread.
            let td = Box::new(read_copy_update::ThreadData::new(thread::current().id()));
            let td_ptr: *mut read_copy_update::ThreadData = Box::into_raw(td);
            $rcu_clone.register_thread(unsafe { &*td_ptr });

            // Execute the thread body.
            let result = {
                $($body)*
            };

            // Unregister the thread before exiting.
            $rcu_clone.unregister_thread(unsafe { &*td_ptr });
            result
        })
    };
}
