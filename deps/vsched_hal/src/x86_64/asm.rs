//! Wrapper functions for assembly instructions.

use core::arch::asm;

use memory_addr::{MemoryAddr, PhysAddr, VirtAddr};
use x86::{controlregs, msr, tlb};
use x86_64::instructions::interrupts;

/// Reads the thread pointer of the current CPU (`FS_BASE`).
///
/// It is used to implement TLS (Thread Local Storage).
#[inline]
pub fn read_thread_pointer() -> usize {
    unsafe { msr::rdmsr(msr::IA32_FS_BASE) as usize }
}

/// Writes the thread pointer of the current CPU (`FS_BASE`).
///
/// It is used to implement TLS (Thread Local Storage).
///
/// # Safety
///
/// This function is unsafe as it changes the CPU states.
#[inline]
pub unsafe fn write_thread_pointer(fs_base: usize) {
    unsafe { msr::wrmsr(msr::IA32_FS_BASE, fs_base as u64) }
}

/// Get the current pc
#[inline]
pub unsafe fn get_pc() -> usize {
    let mut pc = 0usize;
    unsafe {
        asm!(
            "lea {pc}, [rip + 0]",
            pc = out(reg) pc,
        );
    }
    pc
}
