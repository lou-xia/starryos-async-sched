// use core::arch::naked_asm;
use core::arch::global_asm;
use memory_addr::{va, VirtAddr};
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
///
/// On x86_64, callee-saved registers are saved to the kernel stack by the
/// `PUSH` instruction. So that [`rsp`] is the `RSP` after callee-saved
/// registers are pushed, and [`kstack_top`] is the top of the kernel stack
/// (`RSP` before any push).
///
/// [`rsp`]: TaskContext::rsp
/// [`kstack_top`]: TaskContext::kstack_top
#[derive(Debug)]
pub struct TaskContext {
    /// The kernel stack top of the task.
    pub kstack_top: VirtAddr,
    /// `RSP` after all callee-saved registers are pushed.
    pub rsp: u64,
    /// Thread Local Storage (TLS).
    pub fs_base: usize,
}

#[repr(C)]
#[derive(Debug, Default)]
struct ContextSwitchFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    rbx: u64,
    rbp: u64,
    rip: u64,
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
        Self {
            kstack_top: va!(0),
            rsp: 0,
            fs_base: 0,
        }
    }

    /// Setup tls register
    pub fn set_tls(&mut self, tls_area: VirtAddr) {
        self.fs_base = tls_area.as_usize();
    }

    /// Initializes the context for a new task, with the given entry point and
    /// kernel stack.
    pub fn init(&mut self, entry: usize, kstack_top: VirtAddr) {
        unsafe {
            // x86_64 calling convention: the stack must be 16-byte aligned before
            // calling a function. That means when entering a new task (`ret` in `context_switch`
            // is executed), (stack pointer + 8) should be 16-byte aligned.
            let frame_ptr = (kstack_top.as_mut_ptr() as *mut u64).sub(1);
            let frame_ptr = (frame_ptr as *mut ContextSwitchFrame).sub(1);
            core::ptr::write(
                frame_ptr,
                ContextSwitchFrame {
                    rip: entry as _,
                    ..Default::default()
                },
            );
            self.rsp = frame_ptr as u64;
        }
        self.kstack_top = kstack_top;
    }

    /// Switches to another task.
    ///
    /// It first saves the current task's context from CPU to this place, and then
    /// restores the next task's context from `next_ctx` to CPU.
    pub fn switch_to(&mut self, next_ctx: &Self) {
        unsafe {
            // self.fs_base = crate::asm::read_thread_pointer();
            // crate::asm::write_thread_pointer(next_ctx.fs_base);
            context_switch(&mut self.rsp, &next_ctx.rsp)
        }
    }
}

// #[unsafe(naked)]
// unsafe extern "C" fn context_switch(_current_stack: &mut u64, _next_stack: &u64) {
//     naked_asm!(
//         "
//         .code64
//         push    rbp
//         push    rbx
//         push    r12
//         push    r13
//         push    r14
//         push    r15
//         mov     [rdi], rsp

//         mov     rsp, [rsi]
//         pop     r15
//         pop     r14
//         pop     r13
//         pop     r12
//         pop     rbx
//         pop     rbp
//         ret",
//     )
// }

global_asm!(
    "
    context_switch:
        .code64
        push    rbp
        push    rbx
        push    r12
        push    r13
        push    r14
        push    r15
        mov     [rdi], rsp

        mov     rsp, [rsi]
        pop     r15
        pop     r14
        pop     r13
        pop     r12
        pop     rbx
        pop     rbp
        ret",
);

unsafe extern "C" {
    fn context_switch(_current_stack: &mut u64, _next_stack: &u64);
}
