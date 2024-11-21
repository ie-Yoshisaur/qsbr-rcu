#[macro_export]
macro_rules! define_rcu {
    ($instance_name:ident, $getter_fn:ident, $type:ty, $init_value:expr) => {
        // Define a static OnceLock to hold the RCU instance.
        static $instance_name: std::sync::OnceLock<Rcu<$type>> = std::sync::OnceLock::new();

        /// Getter function to access the static RCU instance.
        /// Initializes the RCU instance with the provided initial value if it hasn't been initialized yet.
        ///
        /// # Examples
        ///
        /// ```rust
        /// let rcu = get_rcu_int();
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
