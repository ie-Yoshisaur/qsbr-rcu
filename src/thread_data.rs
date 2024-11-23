use crate::Rcu;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::thread;

/// Thread-specific data structure used to track the state of each thread.
pub struct ThreadData {
    /// Pointer to the next ThreadData in the global list.
    next: AtomicPtr<ThreadData>,
    /// Indicates whether the thread is currently registered.
    registered: AtomicBool,
    /// Indicates whether the thread is within an RCU read-side critical section.
    in_critical: AtomicBool,
}

impl ThreadData {
    /// Creates a new ThreadData instance with default values.
    pub fn new() -> Self {
        ThreadData {
            next: AtomicPtr::new(ptr::null_mut()),
            registered: AtomicBool::new(false),
            in_critical: AtomicBool::new(false),
        }
    }
}

// impl Drop for ThreadData {
//     fn drop(&mut self) {
//         // TODO: Implement thread unregistration.
//     }
// }

thread_local! {
    /// Thread-local storage for each thread's ThreadData.
    static THREAD_DATA: &'static ThreadData = {
        // Allocate and leak a new ThreadData instance to ensure it lives for the thread's lifetime.
        let td = Box::leak(Box::new(ThreadData::new()));
        // Register the thread's ThreadData in the global thread list.
        register_thread(td);
        td
    };
}

/// The head of the global thread list, storing pointers to all registered ThreadData instances.
static THREAD_LIST_HEAD: AtomicPtr<ThreadData> = AtomicPtr::new(ptr::null_mut());

/// Registers a thread by adding its ThreadData to the global thread list.
fn register_thread(td: &'static ThreadData) {
    // Mark the thread as registered.
    td.registered.store(true, Ordering::SeqCst);
    let td_ptr = td as *const _ as *mut _;

    // Insert the new ThreadData at the head of the global list.
    td.next
        .store(THREAD_LIST_HEAD.load(Ordering::SeqCst), Ordering::SeqCst);
    THREAD_LIST_HEAD.store(td_ptr, Ordering::SeqCst);
}

/// Marks the beginning of an RCU read-side critical section.
/// This function sets the `in_critical` flag to true for the current thread.
pub fn rcu_read_lock() {
    THREAD_DATA.with(|td| {
        td.in_critical.store(true, Ordering::SeqCst);
        // Ensure memory ordering to prevent reordering of read operations.
        std::sync::atomic::fence(Ordering::SeqCst);
    });
}

/// Marks the end of an RCU read-side critical section.
/// This function clears the `in_critical` flag for the current thread.
pub fn rcu_read_unlock() {
    THREAD_DATA.with(|td| {
        // Ensure memory ordering before releasing the critical section.
        std::sync::atomic::fence(Ordering::SeqCst);
        td.in_critical.store(false, Ordering::SeqCst);
    });
}

/// Waits until all ongoing RCU read-side critical sections have completed.
/// This ensures that any data being updated can be safely reclaimed.
pub fn synchronize_rcu() {
    loop {
        let mut ptr = THREAD_LIST_HEAD.load(Ordering::SeqCst);
        let mut any_in_critical = false;

        unsafe {
            // Traverse the global thread list to check if any thread is in a critical section.
            while !ptr.is_null() {
                let td = &*ptr;
                if td.registered.load(Ordering::SeqCst) {
                    if td.in_critical.load(Ordering::SeqCst) {
                        any_in_critical = true;
                        break;
                    }
                }
                ptr = td.next.load(Ordering::SeqCst);
            }
        }

        if !any_in_critical {
            // If no threads are in a critical section, synchronization is complete.
            break;
        } else {
            // Yield execution to allow other threads to proceed and potentially exit their critical sections.
            thread::yield_now();
        }
    }
}

pub fn call_rcu<T>(rcu: &Rcu<T>) {
    synchronize_rcu();
    rcu.process_callbacks();
}

/// Safely assigns a new value to an RCU-protected atomic pointer.
/// Ensures memory ordering to make the update visible to other threads.
///
/// # Examples
///
/// ```rust
/// use std::sync::atomic::AtomicPtr;
/// use read_copy_update::rcu_assign_pointer;
/// use std::ptr;
///
/// let atomic_ptr = AtomicPtr::new(ptr::null_mut());
/// let new_ptr = Box::into_raw(Box::new(42));
/// rcu_assign_pointer(&atomic_ptr, new_ptr);
/// ```
pub fn rcu_assign_pointer<T>(p: &AtomicPtr<T>, v: *mut T) {
    // Ensure all previous writes are completed before assigning the new pointer.
    std::sync::atomic::fence(Ordering::SeqCst);
    p.store(v, Ordering::SeqCst);
}

/// Safely dereferences an RCU-protected atomic pointer.
/// Ensures memory ordering to make sure the read is consistent.
///
/// # Examples
///
/// ```rust
/// use std::sync::atomic::AtomicPtr;
/// use crate::read_copy_update::rcu_dereference;
///
/// let atomic_ptr = AtomicPtr::new(Box::into_raw(Box::new(42)));
/// let ptr = rcu_dereference(&atomic_ptr);
/// assert!(!ptr.is_null());
/// unsafe {
///     assert_eq!(*ptr, 42);
/// }
/// ```
pub fn rcu_dereference<T>(p: &AtomicPtr<T>) -> *mut T {
    let ptr = p.load(Ordering::SeqCst);
    // Ensure that the read operation is not reordered with subsequent operations.
    std::sync::atomic::fence(Ordering::SeqCst);
    ptr
}
