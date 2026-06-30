//! Wrapper functions for assembly instructions.

/// Reads the thread pointer of the current CPU (`tp`).
///
/// It is used to implement TLS (Thread Local Storage).
#[inline]
pub fn read_thread_pointer() -> usize {
    let tp;
    unsafe { core::arch::asm!("mv {}, tp", out(reg) tp) };
    tp
}

/// Writes the thread pointer of the current CPU (`tp`).
///
/// It is used to implement TLS (Thread Local Storage).
///
/// # Safety
///
/// This function is unsafe as it changes the CPU states.
#[inline]
pub unsafe fn write_thread_pointer(tp: usize) {
    unsafe { core::arch::asm!("mv tp, {}", in(reg) tp) }
}

/// Get the current pc
#[inline]
pub unsafe fn get_pc() -> usize {
    let mut pc = 0usize;
    unsafe {
        core::arch::asm!(
            "auipc {pc}, 0",
            pc = out(reg) pc,
        );
    };
    pc
}
