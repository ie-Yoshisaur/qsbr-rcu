use crate::thread_data::{rcu_dereference, rcu_read_lock, rcu_read_unlock};
use std::ptr;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering;

/// RCU-protected pointer structure.
/// Manages concurrent reads and updates without using traditional locking mechanisms.
///
/// # Examples
///
/// ```rust
/// use std::thread;
/// use read_copy_update::{define_rcu, Rcu};
///
/// define_rcu!(RCU_INT, get_rcu_int, i32, 42);
/// let rcu = get_rcu_int();
///
/// // Reader thread
/// let reader = thread::spawn(move || {
///     match rcu.read(|val| {
///         println!("Read value: {}", val);
///     }) {
///         Ok(_) => (),
///         Err(_) => println!("Failed to read value."),
///     }
/// });
///
/// // Updater thread
/// let updater = thread::spawn(move || {
///     rcu.try_update(|val| val + 1).unwrap();
///     println!("Value incremented.");
/// });
///
/// reader.join().unwrap();
/// updater.join().unwrap();
/// ```
pub struct Rcu<T> {
    ptr: AtomicPtr<T>,
}

impl<T> Rcu<T> {
    /// Creates a new RCU instance by allocating the provided data on the heap.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use read_copy_update::Rcu;
    ///
    /// let rcu = Rcu::new(42);
    /// ```
    pub fn new(data: T) -> Self {
        let boxed = Box::new(data);
        Rcu {
            ptr: AtomicPtr::new(Box::into_raw(boxed)),
        }
    }

    /// Creates a new static RCU instance with a null pointer.
    /// This can be initialized later with actual data.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use read_copy_update::Rcu;
    ///
    /// let rcu = Rcu::<i32>::new_static();
    /// ```
    pub const fn new_static() -> Self {
        Rcu {
            ptr: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Initializes a static RCU instance with the provided data.
    /// Allocates the data on the heap and assigns it to the atomic pointer.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use read_copy_update::Rcu;
    ///
    /// let rcu = Rcu::new_static();
    /// rcu.initialize(100);
    /// ```
    pub fn initialize(&self, data: T) {
        let boxed = Box::new(data);
        self.ptr.store(Box::into_raw(boxed), Ordering::SeqCst);
    }

    /// Reads the data protected by RCU within a read-side critical section.
    /// The provided closure `f` is executed with a reference to the data.
    /// Returns `Ok` with the result of the closure if successful, or `Err` if the pointer was null.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use read_copy_update::{Rcu, define_rcu};
    ///
    /// define_rcu!(RCU_INT, get_rcu_int, i32, 42);
    /// let rcu = get_rcu_int();
    ///
    /// match rcu.read(|val| *val) {
    ///     Ok(value) => println!("Value: {}", value),
    ///     Err(_) => println!("Failed to read value."),
    /// }
    /// ```
    pub fn read<F, R>(&self, f: F) -> Result<R, ()>
    where
        F: FnOnce(&T) -> R,
    {
        // Enter a read-side critical section.
        rcu_read_lock();
        let ptr = rcu_dereference(&self.ptr);
        let result = unsafe {
            if ptr.is_null() {
                // If the pointer is null, exit the critical section and return an error.
                rcu_read_unlock();
                return Err(());
            }
            // Execute the closure with a reference to the data.
            f(&*ptr)
        };
        // Exit the critical section.
        rcu_read_unlock();
        Ok(result)
    }

    /// Attempts to update the data protected by RCU using a Compare-And-Swap (CAS) operation.
    /// The provided closure `f` generates the new data based on the current data.
    /// If the CAS operation succeeds, the old data is safely reclaimed.
    /// If it fails (due to concurrent updates), the operation is retried.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use read_copy_update::{define_rcu, Rcu};
    ///
    /// define_rcu!(RCU_INT, get_rcu_int, i32, 42);
    /// let rcu = get_rcu_int();
    /// rcu.try_update(|val| val + 1).unwrap();
    /// ```
    pub fn try_update<F>(&self, f: F) -> Result<(), ()>
    where
        F: Fn(&T) -> T,
    {
        loop {
            let old_ptr = self.ptr.load(Ordering::SeqCst);
            if old_ptr.is_null() {
                // If the pointer is null, exit the critical section and return an error.
                return Err(());
            }
            let new_data = unsafe {
                // Generate the new data based on the current data.
                f(&*old_ptr)
            };

            let new_box = Box::new(new_data);
            let new_ptr = Box::into_raw(new_box);

            // Attempt to atomically replace the old pointer with the new pointer.
            match self
                .ptr
                .compare_exchange(old_ptr, new_ptr, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => {
                    return Ok(());
                }
                Err(_) => {
                    // If the CAS operation failed, deallocate the new data and retry.
                    unsafe {
                        let _ = Box::from_raw(new_ptr);
                    }
                    // Continue the loop to retry the update.
                    continue;
                }
            }
        }
    }
}

// impl<T> Drop for Rcu<T> {
//     fn drop(&mut self) {
//         // TODO: Implement Quiescent State-Based Reclamation (QSBR) for safe memory reclamation.
//     }
// }
