// use core::arch::naked_asm;
use core::arch::global_asm;
use memory_addr::VirtAddr;

/// Saved hardware states of a task.
///
/// The context usually includes:
///
/// - Callee-saved registers
/// - Stack pointer register
/// - Thread pointer register (for thread-local storage, currently unsupported)
/// - FP/SIMD registers
///
/// On context switch, current task saves its context from CPU to memory,
/// and the next task restores its context from memory to CPU.
#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Default)]
pub struct TaskContext {
    pub sp: u64,
    pub tpidr_el0: u64,
    pub r19: u64,
    pub r20: u64,
    pub r21: u64,
    pub r22: u64,
    pub r23: u64,
    pub r24: u64,
    pub r25: u64,
    pub r26: u64,
    pub r27: u64,
    pub r28: u64,
    pub r29: u64,
    pub lr: u64, // r30
}

impl TaskContext {
    /// Creates a dummy context for a new task.
    ///
    /// Note the context is not initialized, it will be filled by [`switch_to`]
    /// (for initial tasks) and [`init`] (for regular tasks) methods.
    ///
    /// [`init`]: TaskContext::init
    /// [`switch_to`]: TaskContext::switch_to
    pub fn new() -> Self {
        Self::default()
    }

    /// When under `uspace` feature, kernel will not use this register.
    pub fn set_tls(&mut self, tls_area: VirtAddr) {
        self.tpidr_el0 = tls_area.as_usize() as u64;
    }

    /// Initializes the context for a new task, with the given entry point and
    /// kernel stack.
    pub fn init(&mut self, entry: usize, kstack_top: VirtAddr) {
        self.sp = kstack_top.as_usize() as u64;
        self.lr = entry as u64;
    }

    /// Switches to another task.
    ///
    /// It first saves the current task's context from CPU to this place, and then
    /// restores the next task's context from `next_ctx` to CPU.
    pub fn switch_to(&mut self, next_ctx: &Self) {
        unsafe { context_switch(self, next_ctx) }
    }
}

// #[unsafe(naked)]
// unsafe extern "C" fn context_switch(_current_task: &mut TaskContext, _next_task: &TaskContext) {
//     naked_asm!(
//         "
//         // save old context (callee-saved registers)
//         stp     x29, x30, [x0, 12 * 8]
//         stp     x27, x28, [x0, 10 * 8]
//         stp     x25, x26, [x0, 8 * 8]
//         stp     x23, x24, [x0, 6 * 8]
//         stp     x21, x22, [x0, 4 * 8]
//         stp     x19, x20, [x0, 2 * 8]
//         mov     x19, sp
//         stp     x19, x20, [x0]

//         // restore new context
//         ldp     x19, x20, [x1]
//         mov     sp, x19
//         ldp     x19, x20, [x1, 2 * 8]
//         ldp     x21, x22, [x1, 4 * 8]
//         ldp     x23, x24, [x1, 6 * 8]
//         ldp     x25, x26, [x1, 8 * 8]
//         ldp     x27, x28, [x1, 10 * 8]
//         ldp     x29, x30, [x1, 12 * 8]

//         ret",
//     )
// }

global_asm!(
    "
    context_switch:
        // save old context (callee-saved registers)
        stp     x29, x30, [x0, 12 * 8]
        stp     x27, x28, [x0, 10 * 8]
        stp     x25, x26, [x0, 8 * 8]
        stp     x23, x24, [x0, 6 * 8]
        stp     x21, x22, [x0, 4 * 8]
        stp     x19, x20, [x0, 2 * 8]
        mov     x19, sp
        stp     x19, x20, [x0]

        // restore new context
        ldp     x19, x20, [x1]
        mov     sp, x19
        ldp     x19, x20, [x1, 2 * 8]
        ldp     x21, x22, [x1, 4 * 8]
        ldp     x23, x24, [x1, 6 * 8]
        ldp     x25, x26, [x1, 8 * 8]
        ldp     x27, x28, [x1, 10 * 8]
        ldp     x29, x30, [x1, 12 * 8]

        ret",
);

unsafe extern "C" {
    fn context_switch(_current_task: &mut TaskContext, _next_task: &TaskContext);
}
