/// Defines a static `Rcu<T>` instance and a corresponding getter function.
///
/// This macro simplifies the creation of `Rcu<T>` instances by generating both a static
/// `OnceLock<Rcu<T>>` and a getter function that initializes and retrieves the `Rcu<T>`
/// instance. It ensures that the `Rcu<T>` instance is initialized only once, following
/// the `Read-Copy-Update` (RCU) synchronization mechanism.
///
/// # Parameters
///
/// - `$instance_name`: The identifier for the static `OnceLock<Rcu<T>>` instance.
/// - `$getter_fn`: The identifier for the getter function that returns a reference to the `Rcu<T>` instance.
/// - `$type`: The type parameter `T` for the `Rcu<T>` instance.
/// - `$init_value`: The initial value used to initialize the `Rcu<T>` instance.
///
/// # Usage Example
///
/// ```rust
/// use read_copy_update::define_rcu;
/// use read_copy_update::Rcu;
///
/// // Define a static RCU instance for an `i32` with an initial value of `42`.
/// define_rcu!(RCU_INT, get_rcu_int, i32, 42);
///
/// fn main() {
///     // Retrieve the RCU instance using the generated getter function.
///     let rcu = get_rcu_int();
///
///     // Perform read and update operations using the RCU instance.
///     rcu.read(|value| {
///         println!("Current value: {}", value);
///     }).unwrap();
///
///     rcu.try_update(|value| value + 1).unwrap();
///
///     rcu.read(|value| {
///         println!("Updated value: {}", value);
///     }).unwrap();
/// }
/// ```
///
/// # Generated Items
///
/// This macro generates the following items:
///
/// - A static `OnceLock<Rcu<T>>` instance with the name specified by `$instance_name`.
/// - A public getter function with the name specified by `$getter_fn` that returns a reference
///   to the `Rcu<T>` instance. This function initializes the `Rcu<T>` instance with the provided
///   `$init_value` if it hasn't been initialized yet.
///
/// # Notes
///
/// - Ensure that the `Rcu<T>` type is properly defined and imported within the scope where this
///   macro is used.
/// - This macro leverages `std::sync::OnceLock` to guarantee that the `Rcu<T>` instance is initialized
///   only once, even in the presence of concurrent access.
/// - The generated getter function provides a convenient way to access the `Rcu<T>` instance throughout
///   your codebase without manually managing initialization.
#[macro_export]
macro_rules! define_rcu {
    ($instance_name:ident, $getter_fn:ident, $type:ty, $init_value:expr) => {
        // Define a static OnceLock to hold the RCU instance.
        static $instance_name: std::sync::OnceLock<Rcu<$type>> = std::sync::OnceLock::new();

        /// Retrieves a reference to the static `Rcu<T>` instance.
        ///
        /// This function initializes the `Rcu<T>` instance with the provided initial value if it hasn't been
        /// initialized yet. Subsequent calls will return the already initialized instance.
        ///
        /// # Examples
        ///
        /// ```rust
        /// use read_copy_update::define_rcu;
        /// use read_copy_update::Rcu;
        ///
        /// define_rcu!(RCU_INT, get_rcu_int, i32, 42);
        ///
        /// fn main() {
        ///     let rcu = get_rcu_int();
        ///     rcu.read(|value| {
        ///         println!("Current value: {}", value);
        ///     }).unwrap();
        /// }
        /// ```
        pub fn $getter_fn() -> &'static Rcu<$type> {
            $instance_name.get_or_init(|| {
                let rcu = Rcu::new_static();
                rcu.initialize($init_value);
                rcu
            })
        }
    };
}
