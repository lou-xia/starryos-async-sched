//! VschedTaskImpl — vsched2 Task 接口的 StarryOS 实现。

use alloc::{boxed::Box, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering},
    task::Poll,
};

use axtask::AxTaskRef;
use libvsched2::Stack as _;

use super::{
    from_vsched_state, to_vsched_state,
    stack::VschedStackImpl,
    trapframe::{UserGeneralRegs, UserTrapFrame, UserTrapFrameKind},
};

/// 协程轮询接口。实现该 trait 的任务被 vsched2 作为协程调度。
pub trait CoroutinePoll: Send + Sync {
    fn poll(&self) -> Poll<isize>;
}

/// 封装 AxTaskRef 以适配 vsched2 Task trait。
pub struct VschedTaskImpl {
    pub task: AxTaskRef,
    pub priority: AtomicIsize,
    /// 任务运行特权级。它与 `pid`（所属地址空间）相互独立。
    pub is_kernel: AtomicBool,
    pub pid: AtomicUsize,
    pub wake_generation: AtomicUsize,
    pub is_coroutine: AtomicBool,
    pub return_value: AtomicIsize,
    pub thread_stack_base: AtomicUsize,
    /// 线程栈的 Stack 实现对象指针（`*mut VschedStackImpl`），由 `thread_stack()` 返回
    pub thread_stack_ptr: AtomicUsize,
    pub coroutine: Option<Arc<dyn CoroutinePoll>>,
    /// 用户态 vDSO 基址，`mm.rs` 加载进程时设置，`into_user` 用作 sepc 计算。
    pub user_vdso_base: AtomicUsize,
    /// 已保存的寄存器上下文指针（`*mut UserTrapFrame`），由 trap/yield 入口填入。
    pub trap_frame: AtomicUsize,
    /// 用户态页表根物理地址（仅用户任务设置），`into_user_context` 时切换到该页表。
    pub user_page_table_root: AtomicUsize,
    /// 用户 AddrSpace 裸指针(Arc<Mutex<AddrSpace>>), 用于 copy_mappings_from
    pub user_aspace_ptr: AtomicUsize,
}

impl VschedTaskImpl {
    pub fn new(
        task: AxTaskRef,
        priority: isize,
        pid: usize,
        is_kernel: bool,
        coroutine: Option<Arc<dyn CoroutinePoll>>,
    ) -> Self {
        Self {
            task,
            priority: AtomicIsize::new(priority),
            is_kernel: AtomicBool::new(is_kernel),
            pid: AtomicUsize::new(pid),
            wake_generation: AtomicUsize::new(1),
            is_coroutine: AtomicBool::new(coroutine.is_some()),
            return_value: AtomicIsize::new(0),
            thread_stack_base: AtomicUsize::new(0),
            thread_stack_ptr: AtomicUsize::new(0),
            coroutine,
            trap_frame: AtomicUsize::new(0),
            user_page_table_root: AtomicUsize::new(0),
            user_aspace_ptr: AtomicUsize::new(0),
            user_vdso_base: AtomicUsize::new(0),
        }
    }

    pub fn inner(&self) -> &AxTaskRef {
        &self.task
    }
}

impl libvsched2::Task for VschedTaskImpl {
    fn state(&self) -> libvsched2::TaskState {
        to_vsched_state(self.task.state())
    }

    fn set_state(&self, state: libvsched2::TaskState) -> libvsched2::TaskState {
        to_vsched_state(self.task.swap_state(from_vsched_state(state)))
    }

    fn match_set_state(
        &self,
        state_from_ready: libvsched2::TaskState,
        state_from_running: libvsched2::TaskState,
        state_from_blocked: libvsched2::TaskState,
        state_from_exited: libvsched2::TaskState,
        state_from_blocking: libvsched2::TaskState,
    ) -> libvsched2::TaskState {
        to_vsched_state(self.task.match_set_state(
            from_vsched_state(state_from_ready),
            from_vsched_state(state_from_running),
            from_vsched_state(state_from_blocked),
            from_vsched_state(state_from_exited),
            from_vsched_state(state_from_blocking),
        ))
    }

    fn priority(&self) -> isize {
        self.priority.load(Ordering::Acquire)
    }

    fn is_coroutine(&self) -> bool {
        self.is_coroutine.load(Ordering::Acquire)
    }

    fn is_kernel(&self) -> bool {
        self.is_kernel.load(Ordering::Acquire)
    }

    fn pid(&self) -> usize {
        self.pid.load(Ordering::Acquire)
    }

    fn set_pid(&self, pid: usize) {
        self.pid.store(pid, Ordering::Release);
    }

    /// 保存当前 callee-saved 上下文，进入 raw_thread_entry。
    /// 下次被调度时从此处返回。
    fn resched(&self) {
        unsafe extern "C" {
            fn vsched_yield_trampoline() -> !;
        }
        unsafe { vsched_yield_trampoline() };
    }

    /// 从 `trap_frame` 恢复寄存器上下文。不返回——直接跳转到保存的指令。
    /// 仅内核任务调用（用户任务走 `into_user_context`）。
    fn restore_context(&self) {
        // A normal axtask kernel thread expects axtask::current() to follow
        // every context restore.  TrapHandler is excluded because its
        // temporary thread continuation deliberately runs inside the user
        // task's with_current_task scope.
        if self.is_kernel.load(Ordering::Acquire) && self.coroutine.is_none() {
            axtask::install_current_task_for_external_scheduler(&self.task);
        }
        let tf_ptr = self.trap_frame.load(Ordering::Acquire);
        assert_ne!(tf_ptr, 0, "restore_context: trap_frame is null");
        let tf = unsafe { &*(tf_ptr as *const UserTrapFrame) };
        unsafe { tf.restore_and_jump() };
    }

    fn poll(&self) -> Poll<isize> {
        match self.coroutine.as_ref() {
            Some(coro) => {
                let polled = coro.poll();
                if let Poll::Ready(value) = polled {
                    self.return_value.store(value, Ordering::Release);
                }
                polled
            }
            None => {
                panic!("Cannot poll a thread task: {}", self.task.id_name());
            }
        }
    }

    fn thread_stack(&self) -> *mut () {
        self.thread_stack_ptr.load(Ordering::Acquire) as *mut ()
    }

    fn set_return_value(&self, value: isize) {
        self.return_value.store(value, Ordering::Release);
    }

    fn dealloc(&self) {
        // 用户任务由 ProcessData Arc 管理生命周期，内核任务暂不回收
    }
}

/// First entry for a normal kernel thread managed by vsched2.
///
/// `register_task` builds a frame that enters here on the task's axtask kernel
/// stack.  When the entry closure returns, commit Exited through the existing
/// vsched2 thread context-save path.
extern "C" fn kernel_thread_entry() -> ! {
    let current = libvsched2::current_task_ptr() as *const VschedTaskImpl;
    assert!(!current.is_null(), "kernel_thread_entry: no vsched2 current task");
    let task = unsafe { &*current };
    axtask::run_task_entry_for_external_scheduler(&task.task);

    axhal::asm::disable_irqs();
    use libvsched2::{Task as _, TaskState};
    task.set_state(TaskState::Exited);
    task.resched();
    unreachable!()
}

fn initial_kernel_thread_frame(task: &AxTaskRef) -> Option<Box<UserTrapFrame>> {
    let stack_top = task.kernel_stack_top()?.as_usize();
    let kernel_gp: usize;
    unsafe { core::arch::asm!("mv {}, gp", out(reg) kernel_gp) };

    let mut regs = UserGeneralRegs::default();
    regs.sp = stack_top;
    regs.gp = kernel_gp;
    Some(Box::new(UserTrapFrame {
        regs,
        sepc: kernel_thread_entry as *const () as usize,
        // restore_and_jump does not consume sstatus, but keep an S-mode frame
        // for diagnostics and for any future unified restore implementation.
        sstatus: 1 << 8,
        scause: 0,
        stval: 0,
        kind: UserTrapFrameKind::Trap,
    }))
}

/// 从 vsched2 返回的裸指针还原 AxTaskRef。
pub fn task_from_raw(task: *const ()) -> Option<AxTaskRef> {
    if task.is_null() {
        return None;
    }
    let vti = unsafe { &*(task as *const VschedTaskImpl) };
    Some(vti.task.clone())
}

/// 创建 VschedTaskImpl 并返回裸指针。
pub fn register_task(
    task: AxTaskRef,
    priority: isize,
    pid: usize,
    is_kernel: bool,
    coroutine: Option<Arc<dyn CoroutinePoll>>,
    vdso_base: usize,
) -> *const VschedTaskImpl {
    let is_thread = coroutine.is_none();
    let initial_frame = if is_kernel && is_thread {
        initial_kernel_thread_frame(&task)
    } else {
        None
    };
    let vti = Box::new(VschedTaskImpl::new(
        task,
        priority,
        pid,
        is_kernel,
        coroutine,
    ));
    vti.user_vdso_base.store(vdso_base, Ordering::Release);
    if is_thread {
        vti.thread_stack_ptr.store(
            VschedStackImpl::alloc() as usize,
            Ordering::Release,
        );
    }
    if let Some(frame) = initial_frame {
        vti.trap_frame
            .store(Box::into_raw(frame) as usize, Ordering::Release);
    }
    Box::into_raw(vti)
}
