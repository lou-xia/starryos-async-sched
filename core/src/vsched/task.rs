//! VschedTaskImpl — vsched2 Task 接口的 StarryOS 实现。

use alloc::{boxed::Box, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering},
    task::Poll,
};

use axtask::{AxTaskRef, TaskExt as _};
use kernel_guard::{BaseGuard, IrqSave};
use libvsched2::Stack as _;
use spin::Mutex;

use super::{
    from_vsched_state, to_vsched_state,
    stack::VschedStackImpl,
    trapframe::{UserGeneralRegs, UserTrapFrame, UserTrapFrameKind},
};

/// 协程轮询接口。实现该 trait 的任务被 vsched2 作为协程调度。
pub trait CoroutinePoll: Send + Sync {
    fn poll(&self) -> Poll<isize>;
}

/// 为内核根协程保存其自身的本地中断状态。
///
/// vsched2 调度循环始终关中断；真正进入根协程前恢复该协程上次保存的
/// SIE 状态，正常返回调度器前再保存状态并关中断。这里不能保存一个
/// `IrqSave` guard 对象，因为 IRQ 或主动让权都可能让当前 poll 非局部地
/// 离开，因而不能依赖 `Drop` 执行。
pub struct IrqCorotineWrapper {
    inner: Arc<dyn CoroutinePoll>,
    irq_state: AtomicUsize,
}

impl IrqCorotineWrapper {
    const SIE: usize = 1 << 1;

    fn new(inner: Arc<dyn CoroutinePoll>) -> Self {
        Self {
            inner,
            // 新创建的内核任务默认可被中断；后续每次 poll 都恢复它
            // 上一次离开时实际保存的状态。
            irq_state: AtomicUsize::new(Self::SIE),
        }
    }
}

impl CoroutinePoll for IrqCorotineWrapper {
    fn poll(&self) -> Poll<isize> {
        assert!(
            !axhal::asm::irqs_enabled(),
            "IrqCorotineWrapper must be entered with IRQs disabled"
        );

        let saved = self.irq_state.load(Ordering::Acquire);
        IrqSave::release(saved);
        let result = self.inner.poll();
        let saved = IrqSave::acquire();
        self.irq_state.store(saved, Ordering::Release);
        result
    }
}

/// 封装 AxTaskRef 以适配 vsched2 Task trait。
pub struct VschedTaskImpl {
    pub task: AxTaskRef,
    /// axtask identity visible while this task executes.  A reusable trap
    /// handler changes this owner for every TrapInfo it accepts.
    execution_task: Mutex<Option<AxTaskRef>>,
    pub priority: AtomicIsize,
    /// 任务运行特权级。它与 `pid`（所属地址空间）相互独立。
    pub is_kernel: AtomicBool,
    pub pid: AtomicUsize,
    pub wake_generation: AtomicUsize,
    pub is_coroutine: AtomicBool,
    /// IRQ 将根协程临时提升为线程后，在线程栈已经重新安装、即将恢复
    /// Trap 上下文时据此恢复协程身份。
    resume_to_coroutine: AtomicBool,
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
    /// User vsched2 task whose trap is being serviced by this task.
    pub trap_owner: AtomicUsize,
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
            execution_task: Mutex::new(None),
            priority: AtomicIsize::new(priority),
            is_kernel: AtomicBool::new(is_kernel),
            pid: AtomicUsize::new(pid),
            wake_generation: AtomicUsize::new(1),
            is_coroutine: AtomicBool::new(coroutine.is_some()),
            resume_to_coroutine: AtomicBool::new(false),
            return_value: AtomicIsize::new(0),
            thread_stack_base: AtomicUsize::new(0),
            thread_stack_ptr: AtomicUsize::new(0),
            coroutine,
            trap_frame: AtomicUsize::new(0),
            user_page_table_root: AtomicUsize::new(0),
            user_aspace_ptr: AtomicUsize::new(0),
            user_vdso_base: AtomicUsize::new(0),
            trap_owner: AtomicUsize::new(0),
        }
    }

    pub fn inner(&self) -> &AxTaskRef {
        &self.task
    }

    fn execution_task(&self) -> AxTaskRef {
        self.execution_task
            .lock()
            .as_ref()
            .cloned()
            .unwrap_or_else(|| self.task.clone())
    }

    fn enter_execution_context(&self, install_current: bool) {
        let task = self.execution_task();
        if install_current {
            axtask::install_current_task_for_external_scheduler(&task);
        }
        if let Some(ext) = task.task_ext() {
            ext.on_enter();
        }
    }

    pub fn leave_execution_context(&self) {
        let task = self.execution_task();
        if let Some(ext) = task.task_ext() {
            ext.on_leave();
        }
    }

    /// Binds a reusable kernel handler to the user task whose TrapInfo it is
    /// processing.  The binding remains installed while block_on saves and
    /// restores the handler continuation.
    pub fn bind_execution_task(&self, owner: *const VschedTaskImpl) {
        assert!(!owner.is_null(), "bind_execution_task: null owner");
        let owner = unsafe { &*owner };
        let mut slot = self.execution_task.lock();
        assert!(slot.is_none(), "trap handler already has an execution owner");
        *slot = Some(owner.task.clone());
        drop(slot);
        self.trap_owner
            .store(owner as *const _ as usize, Ordering::Release);
        self.enter_execution_context(true);
    }

    /// Clears the current TrapInfo owner.  Advancing the generation makes any
    /// Waker retained by the completed syscall unable to wake this handler
    /// after it has been reused for a later TrapInfo.
    pub fn unbind_execution_task(&self) {
        self.leave_execution_context();
        self.wake_generation.fetch_add(1, Ordering::AcqRel);
        self.trap_owner.store(0, Ordering::Release);
        let old = self.execution_task.lock().take();
        assert!(old.is_some(), "trap handler has no execution owner");
        axtask::install_current_task_for_external_scheduler(&self.task);
    }

    pub fn has_execution_task(&self) -> bool {
        self.execution_task.lock().is_some()
    }

    /// 将被 IRQ 打断的内核根协程临时提升为线程。
    ///
    /// 此函数在本地中断关闭、进入 vsched2 `trap_entry` 之前调用。
    /// `take_current_stack()` 取出的正是被打断协程正在使用的栈；把这个
    /// 概念上的 `_old` 保存为任务线程栈后，vsched2 随后的
    /// `set_current_stack()` 会有意得到 `None`，避免把同一栈再次当成可回收
    /// 的旧栈。中断处理仍使用 `sscratch` 中的 trap 栈。
    pub fn promote_interrupted_kernel_coroutine(&self) -> bool {
        if !self.is_kernel.load(Ordering::Acquire)
            || !self.is_coroutine.load(Ordering::Acquire)
        {
            return false;
        }
        assert!(
            !self.resume_to_coroutine.load(Ordering::Acquire),
            "interrupted coroutine already awaits restoration"
        );

        let stack = libvsched2::take_current_stack();
        assert!(
            !stack.is_null(),
            "interrupted coroutine has no current stack"
        );
        self.thread_stack_ptr.store(stack as usize, Ordering::Release);
        self.resume_to_coroutine.store(true, Ordering::Release);
        self.is_coroutine.store(false, Ordering::Release);
        true
    }

    /// 在主动让权进入 vsched2 前，把当前 CPU 上安装的线程栈交还给任务。
    ///
    /// `raw_thread_entry` 之后调度器可能在本核等待，也可能让该任务在其它
    /// 核恢复，因此不能继续把保存了 continuation 的栈作为调度器栈使用。
    /// 普通线程已有稳定的 `thread_stack_ptr`；由 `block_on` 临时提升的
    /// 协程则在这里首次取得并登记它刚刚使用的协程栈。
    pub fn detach_thread_stack_for_resched(&self) {
        assert!(
            !self.is_coroutine.load(Ordering::Acquire),
            "cannot detach stack from a coroutine context"
        );

        let stack = libvsched2::take_current_stack();
        assert!(!stack.is_null(), "thread has no current stack to detach");
        let previous = self.thread_stack_ptr.swap(stack as usize, Ordering::AcqRel);
        assert!(
            previous == 0 || previous == stack as usize,
            "thread current stack differs from its saved stack"
        );
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
        // Reusable trap handlers restore the dynamic user execution identity;
        // ordinary kernel tasks restore their own axtask identity.
        let install_current = self.is_kernel.load(Ordering::Acquire);
        self.enter_execution_context(install_current);
        let tf_ptr = self.trap_frame.load(Ordering::Acquire);
        assert_ne!(tf_ptr, 0, "restore_context: trap_frame is null");
        let tf = unsafe { &*(tf_ptr as *const UserTrapFrame) };

        // run_task 已经通过 thread_stack() 把被 IRQ 打断的栈安装为当前
        // current_stack，此后任务可恢复为协程。即使 sret 后立刻再次发生
        // IRQ，新的中断也会再次按同一协议取走这个 current_stack。
        if self.resume_to_coroutine.swap(false, Ordering::AcqRel) {
            assert!(
                !self.is_coroutine.load(Ordering::Acquire),
                "interrupted coroutine was restored twice"
            );
            self.thread_stack_ptr.store(0, Ordering::Release);
            self.is_coroutine.store(true, Ordering::Release);
            // 只有真正被 IRQ 打断的根协程需要应用保存的 sstatus，执行
            // SPIE -> SIE。普通内核线程继续使用既有的直接跳转恢复路径。
            unsafe { tf.restore_and_sret() };
        }
        unsafe { tf.restore_and_jump() };
    }

    fn poll(&self) -> Poll<isize> {
        let install_current = self.is_kernel.load(Ordering::Acquire);
        match self.coroutine.as_ref() {
            Some(coro) => {
                self.enter_execution_context(install_current);
                let polled = coro.poll();
                self.leave_execution_context();
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
        // sret returns to S-mode (SPP).  Preserve the existing migration-stage
        // rule that ordinary kernel threads start with IRQs disabled; only
        // root coroutines opt into interruptible execution through
        // IrqCorotineWrapper.
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
    let coroutine = coroutine.map(|coroutine| {
        if is_kernel {
            Arc::new(IrqCorotineWrapper::new(coroutine)) as Arc<dyn CoroutinePoll>
        } else {
            coroutine
        }
    });
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
