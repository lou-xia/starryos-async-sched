//! Wrapper functions for assembly instructions.

use aarch64_cpu::registers::*;
use core::arch::asm;

/// Reads the thread pointer of the current CPU (`TPIDR_EL0`).
///
/// It is used to implement TLS (Thread Local Storage).
#[inline]
pub fn read_thread_pointer() -> usize {
    TPIDR_EL0.get() as usize
}

/// Writes the thread pointer of the current CPU (`TPIDR_EL0`).
///
/// It is used to implement TLS (Thread Local Storage).
///
/// # Safety
///
/// This function is unsafe as it changes the current CPU states.
#[inline]
pub unsafe fn write_thread_pointer(tpidr_el0: usize) {
    TPIDR_EL0.set(tpidr_el0 as _)
}

/// Get the current pc
#[inline]
pub unsafe fn get_pc() -> usize {
    let mut pc = 0usize;
    unsafe {
        asm!(
            "
            adrp {pc}, 1f
            1: nop
            ",
            pc = out(reg) pc,
        );
    };
    pc
}
