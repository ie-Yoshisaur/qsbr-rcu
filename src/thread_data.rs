use crate::Rcu;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::thread;

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
    /// Indicates whether the thread is currently registered.
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
    static THREAD_DATA: &'static ThreadData = {
        // Allocate and leak a new ThreadData instance to ensure it lives for the thread's lifetime.
        let td = Box::leak(Box::new(ThreadData::new()));
        // Register the thread's ThreadData in the global thread list.
        register_thread(td);
        td
    };
}

/// Global counter counter for synchronization
static GLOBAL_COUNTER: AtomicUsize = AtomicUsize::new(1);

/// The head of the global thread list, storing pointers to all registered ThreadData instances.
static THREAD_LIST_HEAD: AtomicPtr<ThreadData> = AtomicPtr::new(ptr::null_mut());

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
fn register_thread(td: &'static ThreadData) {
    // Mark the thread as registered.
    td.registered.store(true, Ordering::SeqCst);
    let td_ptr = td as *const _ as *mut _;

    // Insert the new ThreadData at the head of the global list.
    td.next
        .store(THREAD_LIST_HEAD.load(Ordering::SeqCst), Ordering::SeqCst);
    THREAD_LIST_HEAD.store(td_ptr, Ordering::SeqCst);
}

/// Marks the end of an RCU read-side critical section.
///
/// This function is intentionally empty in QSBR (Quiescent State Based RCU) implementation
/// because the quiescent state is detected by observing thread's local counter value relative
/// to the global counter. No explicit action is needed when exiting the critical section.
///
/// For symmetry with `rcu_read_lock()`, this function is provided to maintain a clear
/// critical section boundary in the code.
///
/// # Examples
///
/// ```rust
/// use read_copy_update::{rcu_read_lock, rcu_read_unlock};
///
/// // Enter a critical section
/// rcu_read_lock();
///
/// // Access RCU-protected data here...
///
/// // Exit the critical section  
/// rcu_read_unlock();
/// ```
pub fn rcu_read_lock() {
    THREAD_DATA.with(|td| {
        // Mark the thread as within a critical section.
        let global_counter = GLOBAL_COUNTER.load(Ordering::SeqCst);
        td.local_counter.store(global_counter, Ordering::SeqCst);
        // Ensure memory ordering to prevent reordering of read operations.
        std::sync::atomic::fence(Ordering::SeqCst);
    });
}

/// Marks the end of an RCU read-side critical section.
///
/// This function does nothing but is provided for symmetry with `rcu_read_lock`.
///
/// # Examples
///
/// ```rust
/// use read_copy_update::{rcu_read_lock, rcu_read_unlock};
///
/// // Enter a critical section.
/// rcu_read_lock();
///
/// // Perform operations on RCU-protected data here.
///
/// // Exit the critical section.
/// rcu_read_unlock();
/// ```
pub fn rcu_read_unlock() {}

/// Waits until all ongoing RCU read-side critical sections have completed.
///
/// # Implementation Details
///
/// * Increments the global counter atomically
/// * Checks each registered thread's local counter
/// * Thread has passed through a quiescent state if its local counter has caught up
///
/// # Examples
///
/// ```rust
/// use read_copy_update::synchronize_rcu;
///
/// // After updating RCU-protected data:
/// synchronize_rcu();
/// // Now safe to free old data
/// ```
pub fn synchronize_rcu() {
    let latest_counter = GLOBAL_COUNTER.fetch_add(1, Ordering::AcqRel);
    let old_counter = latest_counter - 1;

    loop {
        let mut ptr = THREAD_LIST_HEAD.load(Ordering::SeqCst);
        let mut all_threads_observed = true;

        unsafe {
            // Traverse the global thread list to check if any thread is in a critical section.
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
            // If no threads are in a critical section, synchronization is complete.
            break;
        } else {
            // Yield execution to allow other threads to proceed and potentially exit their critical sections.
            thread::yield_now();
        }
    }
}

/// Processes all pending callbacks for outdated RCU data after ensuring
/// that all ongoing RCU read-side critical sections have completed.
///
/// # Examples
///
/// ```rust
/// use std::sync::Arc;
/// use std::thread;
/// use read_copy_update::{Rcu, call_rcu};
///
/// fn main() {
///     let rcu = Arc::new(Rcu::new(42));
///
///     // Update data
///     rcu.try_update(|val| val + 1).unwrap();
///     println!("Value incremented.");
///
///     // Process callbacks to safely clean up outdated data.
///     call_rcu(&rcu);
///
///     // Verify the updated value
///     rcu.read(|val| {
///         println!("Value after callback processing: {}", val);
///         assert!(*val >= 42);
///     }).unwrap();
/// }
/// ```
pub fn call_rcu<T>(rcu: &Rcu<T>) {
    synchronize_rcu();
    rcu.process_callbacks();
}

/// Safely assigns a new value to an RCU-protected atomic pointer.
///
/// This function ensures that the update is visible to other threads while
/// maintaining proper memory ordering.
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
///
/// // Safely assign the new pointer value.
/// rcu_assign_pointer(&atomic_ptr, new_ptr);
///
/// // Ensure the old pointer is reclaimed appropriately.
/// ```
pub fn rcu_assign_pointer<T>(p: &AtomicPtr<T>, v: *mut T) {
    // Ensure all previous writes are completed before assigning the new pointer.
    std::sync::atomic::fence(Ordering::SeqCst);
    p.store(v, Ordering::SeqCst);
}

/// Safely dereferences an RCU-protected atomic pointer.
///
/// This function ensures that the read is consistent and follows proper
/// memory ordering rules.
///
/// # Examples
///
/// ```rust
/// use std::sync::atomic::AtomicPtr;
/// use read_copy_update::rcu_dereference;
///
/// // Create an atomic pointer to manage RCU-protected data.
/// let atomic_ptr = AtomicPtr::new(Box::into_raw(Box::new(42)));
///
/// // Safely dereference the pointer.
/// let ptr = rcu_dereference(&atomic_ptr);
///
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
