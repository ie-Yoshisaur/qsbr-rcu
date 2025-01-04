use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::thread;

/// A synchronization structure based on Read-Copy-Update (RCU).
///
/// RCU allows readers to access shared data without blocking, while writers perform updates by
/// creating new versions of the data and replacing the old pointer atomically. Readers are guaranteed
/// to see either the old or the new version of the data.
///
/// This implementation also supports deferred cleanup of old data through callbacks.
pub struct Rcu<T> {
    /// Atomic pointer to the current data.
    ptr: AtomicPtr<T>,
    /// Head of the linked list containing thread data.
    thread_list_head: AtomicPtr<ThreadData>,
    /// Head of the linked list containing callbacks for deferred cleanup.
    callbacks: AtomicPtr<Callback<T>>,
    /// Global counter used to track quiescent states.
    global_counter: AtomicUsize,
    /// Lock for synchronizing access to the callback list.
    callback_list_lock: AtomicBool,
    /// Lock for synchronizing access to the thread list.
    thread_list_lock: AtomicBool,
    /// Lock for synchronizing RCU operations.
    sync_lock: AtomicBool,
    /// Unique identifier for the RCU instance.
    rcu_id: usize,
}

/// Global RCU ID counter to assign unique IDs to each RCU instance.
static GLOBAL_RCU_ID: AtomicUsize = AtomicUsize::new(1); // AtomicUsize for thread-safe ID generation.

impl<T> Rcu<T> {
    /// Creates a new RCU instance by allocating the provided data on the heap.
    ///
    /// This method ensures that the data is managed in a way compatible with RCU's requirements.
    ///
    /// Additionally, it automatically registers the creating thread with the new RCU instance.
    ///
    /// # Parameters
    ///
    /// - `data`: The initial data to be managed by RCU.
    pub fn new(data: T) -> Self {
        let boxed = Box::new(data);
        let rcu_id = GLOBAL_RCU_ID.fetch_add(1, Ordering::Relaxed);
        let rcu = Rcu {
            ptr: AtomicPtr::new(Box::into_raw(boxed)),
            callbacks: AtomicPtr::new(ptr::null_mut()),
            global_counter: AtomicUsize::new(1),
            callback_list_lock: AtomicBool::new(false),
            thread_list_lock: AtomicBool::new(false),
            sync_lock: AtomicBool::new(false),
            thread_list_head: AtomicPtr::new(ptr::null_mut()),
            rcu_id,
        };
        rcu.register_thread(); // Register the creating thread.
        rcu
    }

    /// Reads the data protected by RCU in a read-side critical section.
    ///
    /// The provided closure is executed with a reference to the current data. If the pointer is null,
    /// an error is returned. This ensures safe access to RCU-protected data.
    ///
    /// # Parameters
    ///
    /// - `f`: A closure that takes a reference to the data and returns a result.
    ///
    /// # Returns
    ///
    /// - `Ok(R)`: The result of the closure if the data is successfully accessed.
    /// - `Err(())`: An error if the data pointer is null.
    pub fn read<F, R>(&self, f: F) -> Result<R, ()>
    where
        F: FnOnce(&T) -> R,
    {
        self.rcu_read_lock(); // Begin read-side critical section.
        let ptr = self.rcu_dereference();
        let result = unsafe {
            if ptr.is_null() {
                self.rcu_read_unlock();
                return Err(());
            }
            f(&*ptr)
        };
        self.rcu_read_unlock(); // End read-side critical section.
        Ok(result)
    }

    /// Marks the beginning of an RCU read-side critical section.
    ///
    /// This function initializes the thread's `local_counter` with the current `global_counter`.
    /// It should be called before accessing RCU-protected data.
    fn rcu_read_lock(&self) {
        THREAD_LOCAL_STORAGE.with(|tls_head| {
            let mut current = tls_head.load(Ordering::Acquire);
            while !current.is_null() {
                let tls = unsafe { &*current };
                if tls.rcu_id == self.rcu_id {
                    let thread_data = unsafe { &*tls.thread_data };
                    thread_data.local_counter.store(
                        self.global_counter.load(Ordering::Acquire),
                        Ordering::Release,
                    );
                    break;
                }
                current = tls.next.load(Ordering::Acquire);
            }
        });
    }

    /// Marks the end of an RCU read-side critical section.
    ///
    /// This function does nothing but is provided for symmetry with `rcu_read_lock`.
    fn rcu_read_unlock(&self) {} // No action needed; the critical section is managed via the counter.

    /// Safely dereferences an RCU-protected atomic pointer.
    ///
    /// This function ensures that the read is consistent and follows proper
    /// memory ordering rules.
    ///
    /// # Returns
    ///
    /// - A raw pointer to the current data.
    fn rcu_dereference(&self) -> *mut T {
        let ptr = self.ptr.load(Ordering::Acquire);
        ptr
    }

    /// Updates the RCU-protected data using the provided closure.
    ///
    /// This method atomically replaces the current data with new data generated by the closure. The
    /// old data is safely reclaimed using deferred cleanup.
    ///
    /// # Parameters
    ///
    /// - `f`: A closure that takes a reference to the current data and returns the updated data.
    ///
    /// # Returns
    ///
    /// - `Ok(())`: If the update is successful.
    /// - `Err(())`: If the current data pointer is null.
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
                .compare_exchange(old_ptr, new_ptr, Ordering::Release, Ordering::Acquire)
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
        self.lock_callback_list();
        unsafe {
            let head = self.callbacks.load(Ordering::Acquire);
            (*cb_ptr).next.store(head, Ordering::Relaxed);
            self.callbacks.store(cb_ptr, Ordering::Release);
        }
        self.unlock_callback_list();
    }

    /// Marks the current thread as having reached a quiescent state.
    ///
    /// This function should be called periodically by reader threads to indicate that they have
    /// completed their current read-side critical section and are in a quiescent state.
    pub fn rcu_quiescent_state(&self) {
        THREAD_LOCAL_STORAGE.with(|tls_head| {
            let mut current = tls_head.load(Ordering::Acquire);
            while !current.is_null() {
                let tls = unsafe { &*current };
                if tls.rcu_id == self.rcu_id {
                    let thread_data = unsafe { &*tls.thread_data };
                    thread_data.local_counter.store(
                        self.global_counter.load(Ordering::Acquire),
                        Ordering::Release,
                    );
                    break;
                }
                current = tls.next.load(Ordering::Acquire);
            }
        });
    }

    /// Initiates garbage collection by processing safe callbacks and cleaning up thread data.
    ///
    /// This method should be called periodically by writer threads to reclaim memory or perform
    /// other cleanup tasks for outdated RCU-protected data.
    pub fn gc(&self) {
        let safe_callbacks = self.synchronize_rcu();
        self.process_callbacks(safe_callbacks);
        self.clean_thread_list();
    }

    /// Processes all pending callbacks and executes them.
    ///
    /// This function should be called periodically to reclaim memory or perform other cleanup tasks
    /// for outdated RCU-protected data.
    ///
    /// # Parameters
    ///
    /// - `callbacks`: A pointer to the head of the callback list to process.
    fn process_callbacks(&self, callbacks: *mut Callback<T>) {
        let mut cb_ptr = callbacks;
        while !cb_ptr.is_null() {
            unsafe {
                let cb = Box::from_raw(cb_ptr);
                (cb.func)(cb.data);
                cb_ptr = cb.next.load(Ordering::Relaxed);
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
    ///
    /// # Returns
    ///
    /// - A pointer to the list of safe callbacks to be processed.
    fn synchronize_rcu(&self) -> *mut Callback<T> {
        self.lock_sync();
        self.lock_thread_list();
        self.rcu_quiescent_state();
        let old_counter = self.global_counter.fetch_add(1, Ordering::AcqRel);

        let old_head = self.callbacks.load(Ordering::Acquire);

        loop {
            let mut all_threads_observed = true;

            unsafe {
                let mut ptr = self.thread_list_head.load(Ordering::Acquire);
                while !ptr.is_null() {
                    let thread_data = &*ptr;
                    if !thread_data.active.load(Ordering::Acquire) {
                        ptr = thread_data.next.load(Ordering::Acquire);
                        continue;
                    }

                    let local_counter = thread_data.local_counter.load(Ordering::Acquire);
                    if local_counter < old_counter {
                        all_threads_observed = false;
                        break;
                    }
                    ptr = thread_data.next.load(Ordering::Acquire);
                }
            }

            if all_threads_observed {
                break;
            } else {
                thread::yield_now();
            }
        }

        self.unlock_thread_list();
        self.unlock_sync();

        self.lock_callback_list();

        let current_head = self.callbacks.load(Ordering::Acquire);

        if old_head.is_null() {
            return ptr::null_mut();
        }

        if old_head == current_head {
            let safe_callback_list = self.callbacks.swap(ptr::null_mut(), Ordering::Acquire);
            self.unlock_callback_list();
            return safe_callback_list;
        }

        let mut prev_ptr = current_head;
        let mut found = false;
        unsafe {
            while !prev_ptr.is_null() {
                let cb = &*prev_ptr;
                let next_ptr = cb.next.load(Ordering::Acquire);
                if next_ptr == old_head {
                    cb.next.store(ptr::null_mut(), Ordering::Release);
                    found = true;
                    break;
                }
                prev_ptr = next_ptr;
            }
        }

        self.unlock_callback_list();

        let safe_callback_list = if found { old_head } else { ptr::null_mut() };
        safe_callback_list
    }

    fn lock_callback_list(&self) {
        loop {
            let is_locked = self.callback_list_lock.load(Ordering::Acquire);
            if !is_locked {
                if self
                    .callback_list_lock
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    break;
                }
            } else {
                thread::yield_now();
            }
        }
    }

    fn unlock_callback_list(&self) {
        self.callback_list_lock.store(false, Ordering::Release);
    }

    fn lock_thread_list(&self) {
        loop {
            let is_locked = self.thread_list_lock.load(Ordering::Acquire);
            if !is_locked {
                if self
                    .thread_list_lock
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    break;
                }
            } else {
                thread::yield_now();
            }
        }
    }

    fn unlock_thread_list(&self) {
        self.thread_list_lock.store(false, Ordering::Release);
    }

    fn lock_sync(&self) {
        loop {
            let is_locked = self.sync_lock.load(Ordering::Acquire);
            if !is_locked {
                if self
                    .sync_lock
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    break;
                }
            } else {
                thread::yield_now();
            }
        }
    }

    fn unlock_sync(&self) {
        self.sync_lock.store(false, Ordering::Release);
    }

    /// Registers the current thread to participate in RCU operations.
    ///
    /// This function should be called when a thread starts using RCU-protected data to ensure that
    /// it is tracked for quiescent state detection.
    ///
    /// # Safety
    ///
    /// - The `ThreadData` reference must remain valid for the lifetime of the thread.
    /// - This function is safe to call multiple times but will only register a thread once per RCU instance.
    pub fn register_thread(&self) {
        THREAD_LOCAL_STORAGE.with(|tls_head| {
            let mut current = tls_head.load(Ordering::Acquire);
            while !current.is_null() {
                let tls = unsafe { &*current };
                if tls.rcu_id == self.rcu_id {
                    return;
                }
                current = tls.next.load(Ordering::Acquire);
            }

            let new_thread_data = Box::new(ThreadData::new());
            let new_thread_data_ptr = Box::into_raw(new_thread_data);

            let new_tls = Box::new(ThreadLocalStorage::new(self.rcu_id, new_thread_data_ptr));
            let new_tls_ptr = Box::into_raw(new_tls);

            loop {
                let head = tls_head.load(Ordering::Acquire);
                unsafe {
                    (*new_tls_ptr).next.store(head, Ordering::Relaxed);
                }
                if tls_head
                    .compare_exchange(head, new_tls_ptr, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    break;
                }
            }

            // Store the pointer to ThreadLocalStorage in ThreadData for cleanup
            unsafe {
                (*new_thread_data_ptr).tls_ptr = new_tls_ptr;
            }

            self.lock_thread_list();
            unsafe {
                (*new_thread_data_ptr).next.store(
                    self.thread_list_head.load(Ordering::Acquire),
                    Ordering::Relaxed,
                );
                self.thread_list_head
                    .store(new_thread_data_ptr, Ordering::Release);
                (*new_thread_data_ptr).active.store(true, Ordering::Release);
            }
            self.unlock_thread_list();
        });
    }

    /// Unregisters the current thread from participating in RCU operations.
    ///
    /// This function should be called when a thread no longer needs to participate in RCU operations,
    /// typically when the thread is about to exit.
    pub fn unregister_thread(&self) {
        THREAD_LOCAL_STORAGE.with(|tls_head| {
            let mut current = tls_head.load(Ordering::Acquire);
            while !current.is_null() {
                let tls = unsafe { &*current };
                if tls.rcu_id == self.rcu_id {
                    unsafe {
                        (*tls.thread_data).active.store(false, Ordering::Release);
                        remove_thread_local_storage(tls_head, tls);
                        drop(Box::from_raw(current));
                    }
                    break;
                }
                current = tls.next.load(Ordering::Acquire);
            }
        });
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
            let thread_data = unsafe { &*current_ptr };
            if !thread_data.active.load(Ordering::Acquire) {
                let next = thread_data.next.load(Ordering::Acquire);
                unsafe {
                    (*prev_ptr).store(next, Ordering::Release);
                    drop(Box::from_raw(current_ptr));
                }
                current_ptr = next;
            } else {
                prev_ptr =
                    unsafe { &(*current_ptr).next as *const _ as *mut AtomicPtr<ThreadData> };
                current_ptr = thread_data.next.load(Ordering::Acquire);
            }
        }
        self.unlock_thread_list();
    }
}

impl<T> Drop for Rcu<T> {
    fn drop(&mut self) {
        self.gc();

        let ptr = self.ptr.swap(ptr::null_mut(), Ordering::Release);
        if !ptr.is_null() {
            unsafe {
                drop(Box::from_raw(ptr)); // Drop the data to free memory.
            }
        }

        let mut current = self.thread_list_head.load(Ordering::Acquire);
        while !current.is_null() {
            let thread_data = unsafe { &*current };

            // Remove the corresponding ThreadLocalStorage
            if !thread_data.tls_ptr.is_null() {
                unsafe {
                    let tls = &mut *thread_data.tls_ptr;
                    THREAD_LOCAL_STORAGE.with(|tls_head| {
                        remove_thread_local_storage(tls_head, tls); // Remove TLS from the thread's storage.
                    });
                    drop(Box::from_raw(thread_data.tls_ptr));
                }
            }
            let next = thread_data.next.load(Ordering::Acquire);
            unsafe {
                drop(Box::from_raw(current));
            }
            current = next;
        }
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
    func: fn(*mut T),             // Function pointer for the callback.
    data: *mut T,                 // Data to be passed to the callback.
    next: AtomicPtr<Callback<T>>, // AtomicPtr for the next callback in the list.
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
fn free_callback<T>(ptr: *mut T) {
    unsafe {
        drop(Box::from_raw(ptr)); // Safely deallocate the data.
    }
}

thread_local! {
    /// Thread-local storage for each thread's `ThreadLocalStorage` linked list.
    ///
    /// Each thread maintains a linked list of `ThreadLocalStorage` instances, one for each RCU instance
    /// it participates in. This structure is essential for implementing multiple RCU instances per thread.
    static THREAD_LOCAL_STORAGE: AtomicPtr<ThreadLocalStorage> = AtomicPtr::new(ptr::null_mut()); // AtomicPtr for thread-safe access to TLS.
}

/// Structure representing thread-local storage for an RCU instance.
///
/// Each `ThreadLocalStorage` contains its own `ThreadData`, a unique `rcu_id`, and a pointer to the next
/// `ThreadLocalStorage` in the thread's linked list.
pub struct ThreadLocalStorage {
    /// Pointer to the thread's `ThreadData`.
    thread_data: *mut ThreadData, // Raw pointer to ThreadData.
    /// Unique identifier for the RCU instance.
    rcu_id: usize, // Unique identifier to match with RCU instances.
    /// Pointer to the next `ThreadLocalStorage` in the linked list.
    next: AtomicPtr<ThreadLocalStorage>, // AtomicPtr for the next TLS in the list.
}

impl ThreadLocalStorage {
    /// Creates a new `ThreadLocalStorage` with the specified RCU ID and `ThreadData` pointer.
    ///
    /// # Parameters
    ///
    /// - `rcu_id`: The unique identifier for the RCU instance.
    /// - `thread_data_ptr`: The pointer to the `ThreadData` associated with this RCU.
    fn new(rcu_id: usize, thread_data_ptr: *mut ThreadData) -> Self {
        ThreadLocalStorage {
            thread_data: thread_data_ptr, // Initialize with the provided ThreadData pointer.
            rcu_id,
            next: AtomicPtr::new(ptr::null_mut()), // Initialize next as null.
        }
    }
}

/// Thread-specific data structure used to track the state of each thread in RCU operations.
///
/// This structure is key to the QSBR (Quiescent State Based RCU) implementation. Each thread
/// maintains its own counter that is compared against the global counter to determine if the
/// thread has passed through a quiescent state since a particular write operation.
///
/// # Fields
///
/// * `local_counter` - Tracks the thread's local view of the global counter.
/// * `active` - Indicates whether the thread is currently active in RCU operations.
/// * `next` - Pointer to the next `ThreadData` in the Rcu's thread list.
/// * `tls_ptr` - Pointer to the corresponding `ThreadLocalStorage`.
pub struct ThreadData {
    /// Tracks the thread's local view of the global counter.
    local_counter: AtomicUsize, // AtomicUsize for atomic updates of the local counter.
    /// Indicates whether the thread is active or not.
    active: AtomicBool, // AtomicBool to indicate if the thread is active.
    /// Pointer to the next `ThreadData` in the Rcu's thread list.
    next: AtomicPtr<ThreadData>, // AtomicPtr for the next ThreadData in the list.
    /// Pointer to the corresponding `ThreadLocalStorage`.
    tls_ptr: *mut ThreadLocalStorage, // Raw pointer to the associated TLS.
}

impl ThreadData {
    /// Creates a new `ThreadData` instance with default values.
    ///
    /// Initializes the `local_counter` to `0` and marks the thread as active.
    fn new() -> Self {
        ThreadData {
            local_counter: AtomicUsize::new(0), // Initialize local counter to 0.
            active: AtomicBool::new(true),      // Mark the thread as active.
            next: AtomicPtr::new(ptr::null_mut()), // Initialize next as null.
            tls_ptr: ptr::null_mut(),           // Initialize TLS pointer as null.
        }
    }
}

/// Retrieves the current thread's `ThreadLocalStorage`.
///
/// This function provides access to the thread-local `ThreadLocalStorage` linked list, which is
/// used internally by the RCU implementation to track the thread's state for multiple RCU instances.
///
/// # Returns
///
/// - A raw pointer to the head of the thread's `ThreadLocalStorage` linked list.
pub fn get_current_thread_storage() -> *mut ThreadLocalStorage {
    THREAD_LOCAL_STORAGE.with(|tls_head| tls_head.load(Ordering::Acquire))
}

/// Removes a specific `ThreadLocalStorage` from the thread's `THREAD_LOCAL_STORAGE` linked list.
///
/// # Parameters
///
/// - `tls_head`: A reference to the thread's `THREAD_LOCAL_STORAGE` head pointer.
/// - `tls_to_remove`: A reference to the `ThreadLocalStorage` that needs to be removed.
fn remove_thread_local_storage(
    tls_head: &AtomicPtr<ThreadLocalStorage>,
    tls_to_remove: &ThreadLocalStorage,
) {
    let mut current = tls_head.load(Ordering::Acquire);
    let mut prev: *mut AtomicPtr<ThreadLocalStorage> =
        tls_head as *const _ as *mut AtomicPtr<ThreadLocalStorage>; // Pointer to the previous node's next pointer.

    while !current.is_null() {
        let current_ref = unsafe { &*current };
        if current_ref.rcu_id == tls_to_remove.rcu_id
            && current_ref.thread_data == tls_to_remove.thread_data
        {
            unsafe {
                let next = current_ref.next.load(Ordering::Acquire);
                (*prev).store(next, Ordering::Release);
                break;
            }
        }
        prev = &current_ref.next as *const _ as *mut AtomicPtr<ThreadLocalStorage>; // Move to the next node.
        current = current_ref.next.load(Ordering::Acquire);
    }
}

/// Marks the current thread as no longer participating in RCU operations.
///
/// This function should be called when a thread is about to terminate or when it no longer
/// needs to participate in RCU operations. It updates the thread's `active` status accordingly.
pub fn drop_thread_data() {
    THREAD_LOCAL_STORAGE.with(|tls_head| {
        let mut current = tls_head.load(Ordering::Acquire);
        while !current.is_null() {
            let tls = unsafe { &*current };
            unsafe {
                (*tls.thread_data).active.store(false, Ordering::Release);
            }
            current = tls.next.load(Ordering::Acquire);
        }
    });
}

/// A macro for spawning a new thread that automatically registers and unregisters its
/// `ThreadLocalStorage` with the provided `Rcu` instance.
///
/// This macro ensures that the thread participates in RCU operations by registering its
/// `ThreadLocalStorage` upon creation and unregistering it before termination.
///
/// # Parameters
///
/// - `$rcu_clone`: An expression that evaluates to a cloned reference of the `Rcu` instance.
/// - `$body`: The body of the thread, provided as a block of code.
/// ```
#[macro_export]
macro_rules! rcu_thread_spawn {
    ($rcu_clone:expr, $($body:tt)*) => {
        std::thread::spawn(move || {
            $rcu_clone.register_thread();
            let result = { $($body)* };
            $rcu_clone.unregister_thread();
            result
        })
    };
}
