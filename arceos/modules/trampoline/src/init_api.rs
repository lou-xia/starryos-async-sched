use alloc::boxed::Box;

/// Initializes the trampoline (for the primary CPU).
pub fn init_trampoline() {
    executor::init(|| Box::pin(crate::user_task_top()));
}

#[cfg(feature = "smp")]
/// Initializes the trampoline for secondary CPUs.
pub fn init_trampoline_secondary() {
    executor::init_secondary();
}
