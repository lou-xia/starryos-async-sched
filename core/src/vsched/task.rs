//! VschedTaskImpl — vsched2 Task 接口的 StarryOS 实现。

use alloc::{boxed::Box, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering},
    task::Poll,
};

use axtask::{AxTaskRef, TaskExt as _};
use hashbrown::HashMap;
use kernel_guard::{BaseGuard, IrqSave};
use libvsched2::Stack as _;
use lazy_static::lazy_static;
use spin::Mutex;
use vsched_abi::{
    UserTaskKey, VschedProcessId, decode_task, encode_task, SHARED_CONTEXT_COROUTINE,
    SHARED_CONTEXT_THREAD, SHARED_TASK_BLOCKED, SHARED_TASK_BLOCKING, SHARED_TASK_EXITED,
    SHARED_TASK_READY, SHARED_TASK_RUNNING,
};

use super::{
    from_vsched_state,
    stack::VschedStackImpl,
    to_vsched_state,
    trapframe::{UserGeneralRegs, UserTrapFrame, UserTrapFrameKind},
};
use crate::task::AsThread;

lazy_static! {
    /// 内核侧稳定句柄到 StarryOS 任务实现的映射。
    ///
    /// 共享 vVAR 只保存用户可观察投影，内核对象始终留在本表中，避免把 `AxTaskRef` 或
    /// 内核裸指针暴露给用户地址空间。
    static ref USER_TASK_REGISTRY: Mutex<HashMap<UserTaskKey, usize>> =
        Mutex::new(HashMap::new());
}

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
    /// 该任务只承载调度器空闲等待期间的中断上下文，不进入普通就绪队列。
    is_scheduler_wait_context: AtomicBool,
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
    /// 共享任务句柄。它既是内核 registry 的键，也会编码为用户调度器中的任务 ID。
    shared_task_key: Mutex<Option<UserTaskKey>>,
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
            is_scheduler_wait_context: AtomicBool::new(false),
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
            shared_task_key: Mutex::new(None),
        }
    }

    pub fn inner(&self) -> &AxTaskRef {
        &self.task
    }

    /// 返回当前任务的共享句柄。
    pub fn shared_task_key(&self) -> Option<UserTaskKey> {
        *self.shared_task_key.lock()
    }

    /// 返回放入用户调度器的任务 ID。
    pub fn task_id(&self) -> Option<*const ()> {
        encode_task(self.shared_task_key()?)
    }

    fn install_shared_task_key(&self, key: UserTaskKey) {
        let mut current = self.shared_task_key.lock();
        assert!(
            current.is_none(),
            "install_shared_task_key: task already has a shared key"
        );
        *current = Some(key);
    }

    fn take_shared_task_key(&self) -> Option<UserTaskKey> {
        self.shared_task_key.lock().take()
    }

    /// 同步任务的协程或线程表示。
    pub fn set_coroutine(&self, is_coroutine: bool) {
        self.is_coroutine.store(is_coroutine, Ordering::Release);
        if let Some(key) = self.shared_task_key() {
            let kind = if is_coroutine {
                SHARED_CONTEXT_COROUTINE
            } else {
                SHARED_CONTEXT_THREAD
            };
            assert!(
                crate::vsched::shared_task_table().set_context_kind(key, kind),
                "set_coroutine: shared task is stale",
            );
        }
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
        assert!(
            slot.is_none(),
            "trap handler already has an execution owner"
        );
        *slot = Some(owner.task.clone());
        drop(slot);
        self.trap_owner
            .store(
                owner
                    .task_id()
                    .unwrap_or(owner as *const _ as *const ()) as usize,
                Ordering::Release,
            );
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

    /// 将任务标记为 vsched2 为当前核心创建的调度器等待上下文。
    pub fn mark_scheduler_wait_context(&self) {
        assert!(
            self.is_kernel.load(Ordering::Acquire),
            "scheduler wait context must be a kernel task",
        );
        assert!(
            self.is_coroutine.load(Ordering::Acquire),
            "scheduler wait context must use an unpolled coroutine container",
        );
        assert!(
            !self
                .is_scheduler_wait_context
                .swap(true, Ordering::AcqRel),
            "scheduler wait context was marked twice",
        );
    }

    /// 在 IRQ 进入 vsched2 前交接被打断执行流正在使用的栈。
    pub fn prepare_interrupted_kernel_context(&self) -> bool {
        if self
            .is_scheduler_wait_context
            .load(Ordering::Acquire)
        {
            // 等待上下文没有需要恢复的 continuation。每次 WFI 被打断时都要
            // 取走本轮调度栈，随后由 vsched2 将它轮换为下一次 trap 栈。
            let stack = libvsched2::take_current_stack();
            assert!(
                !stack.is_null(),
                "scheduler wait context has no current stack",
            );
            self.thread_stack_ptr.store(stack as usize, Ordering::Release);
            return true;
        }

        self.promote_interrupted_kernel_coroutine()
    }

    /// 将被 IRQ 打断的内核根协程临时提升为线程。
    ///
    /// 此函数在本地中断关闭、进入 vsched2 `trap_entry` 之前调用。
    /// `take_current_stack()` 取出的正是被打断协程正在使用的栈；把这个
    /// 概念上的 `_old` 保存为任务线程栈后，vsched2 随后的
    /// `set_current_stack()` 会有意得到 `None`，避免把同一栈再次当成可回收
    /// 的旧栈。中断处理仍使用 `sscratch` 中的 trap 栈。
    pub fn promote_interrupted_kernel_coroutine(&self) -> bool {
        if !self.is_kernel.load(Ordering::Acquire) || !self.is_coroutine.load(Ordering::Acquire) {
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
        self.thread_stack_ptr
            .store(stack as usize, Ordering::Release);
        self.resume_to_coroutine.store(true, Ordering::Release);
        self.set_coroutine(false);
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
        if let Some(key) = self.shared_task_key()
            && let Some(state) = crate::vsched::shared_task_table().task_state(key)
        {
            return state_from_raw(state);
        }
        to_vsched_state(self.task.state())
    }

    fn set_state(&self, state: libvsched2::TaskState) -> libvsched2::TaskState {
        if let Some(key) = self.shared_task_key() {
            let state = state_to_raw(state);
            let old = crate::vsched::shared_task_table()
                .swap_task_state(key, state)
                .expect("set_state: shared task is stale");
            self.task.set_state(from_vsched_state(state_from_raw(state)));
            return state_from_raw(old);
        }
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
        if let Some(key) = self.shared_task_key() {
            let states = [
                state_to_raw(state_from_ready),
                state_to_raw(state_from_running),
                state_to_raw(state_from_blocked),
                state_to_raw(state_from_exited),
                state_to_raw(state_from_blocking),
            ];
            let old = crate::vsched::shared_task_table()
                .match_set_task_state(
                    key, states[0], states[1], states[2], states[3], states[4],
                )
                .expect("match_set_state: shared task is stale");
            let next = match old {
                SHARED_TASK_READY => states[0],
                SHARED_TASK_RUNNING => states[1],
                SHARED_TASK_BLOCKED => states[2],
                SHARED_TASK_EXITED => states[3],
                SHARED_TASK_BLOCKING => states[4],
                _ => unreachable!(),
            };
            self.task.set_state(from_vsched_state(state_from_raw(next)));
            return state_from_raw(old);
        }
        to_vsched_state(self.task.match_set_state(
            from_vsched_state(state_from_ready),
            from_vsched_state(state_from_running),
            from_vsched_state(state_from_blocked),
            from_vsched_state(state_from_exited),
            from_vsched_state(state_from_blocking),
        ))
    }

    fn priority(&self) -> isize {
        if let Some(key) = self.shared_task_key()
            && let Some(priority) = crate::vsched::shared_task_table().priority(key)
        {
            return priority;
        }
        self.priority.load(Ordering::Acquire)
    }

    fn is_coroutine(&self) -> bool {
        if let Some(key) = self.shared_task_key()
            && let Some(kind) = crate::vsched::shared_task_table().context_kind(key)
        {
            return kind == SHARED_CONTEXT_COROUTINE;
        }
        self.is_coroutine.load(Ordering::Acquire)
    }

    fn is_kernel(&self) -> bool {
        self.is_kernel.load(Ordering::Acquire)
    }

    fn pid(&self) -> usize {
        if let Some(key) = self.shared_task_key()
            && let Some(pid) = crate::vsched::shared_task_table().process_id(key)
        {
            return pid.as_raw();
        }
        self.pid.load(Ordering::Acquire)
    }

    fn set_pid(&self, pid: usize) {
        self.pid.store(pid, Ordering::Release);
        if let Some(key) = self.shared_task_key() {
            let pid = VschedProcessId::from_user_raw(pid)
                .expect("set_pid: user task received a reserved process id");
            assert!(
                crate::vsched::shared_task_table().set_process_id(key, pid),
                "set_pid: shared task is stale",
            );
        }
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
            self.set_coroutine(true);
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
        self.dealloc_task(true);
    }
}

impl VschedTaskImpl {
    fn dealloc_task(&self, unregister: bool) {
        assert!(
            !self
                .is_scheduler_wait_context
                .load(Ordering::Acquire),
            "scheduler wait context must live for the lifetime of its CPU",
        );
        assert_eq!(
            to_vsched_state(self.task.state()),
            libvsched2::TaskState::Exited,
            "dealloc requires an exited task",
        );
        assert!(
            !self.has_execution_task(),
            "cannot deallocate a task with an active execution owner",
        );
        assert_eq!(
            self.trap_owner.load(Ordering::Acquire),
            0,
            "cannot deallocate a task still bound to a trap owner",
        );

        self.wake_generation.fetch_add(1, Ordering::AcqRel);

        // 先从内核 registry 和共享 vVAR 表移除句柄，再释放任务对象本身。这样旧的
        // Waker 或用户句柄都只能观察到失效的 generation。
        if unregister {
            unregister_user_task(self as *const Self);
        }

        let trap_frame = self.trap_frame.swap(0, Ordering::AcqRel);
        if trap_frame != 0 {
            unsafe { drop(Box::from_raw(trap_frame as *mut UserTrapFrame)) };
        }

        let thread_stack = self.thread_stack_ptr.swap(0, Ordering::AcqRel);
        if thread_stack != 0 {
            unsafe { (&mut *(thread_stack as *mut VschedStackImpl)).dealloc() };
        }

        // A process slot stores a borrowed AddrSpace pointer.  Remove the slot
        // before dropping the last Thread/ProcessData owner of that address space.
        if !self.is_kernel.load(Ordering::Acquire)
            && let Some(thread) = self.task.try_as_thread()
            && thread.proc_data.proc.threads().is_empty()
        {
            if let Some(pid) = thread.proc_data.take_vsched_process_id() {
                super::process_drop(pid.as_raw());
            }
        }

        let ptr = self as *const Self as *mut Self;
        unsafe { drop(Box::from_raw(ptr)) };
    }
}

/// 将 vsched2 的任务地址分发到直接任务或共享用户任务。
pub struct TaskImpl;

impl TaskImpl {
    fn raw(&self) -> *const () {
        self as *const Self as *const ()
    }

    fn task<R>(&self, f: impl FnOnce(&VschedTaskImpl) -> R) -> R {
        with_vsched_task(self.raw(), f).expect("TaskImpl: invalid task")
    }

    fn direct(&self) -> &VschedTaskImpl {
        let task = direct_task(self.raw()).expect("TaskImpl: invalid task");
        unsafe { &*task }
    }
}

impl libvsched2::Task for TaskImpl {
    fn state(&self) -> libvsched2::TaskState {
        self.task(libvsched2::Task::state)
    }

    fn set_state(&self, state: libvsched2::TaskState) -> libvsched2::TaskState {
        self.task(|task| task.set_state(state))
    }

    fn match_set_state(
        &self,
        state_from_ready: libvsched2::TaskState,
        state_from_running: libvsched2::TaskState,
        state_from_blocked: libvsched2::TaskState,
        state_from_exited: libvsched2::TaskState,
        state_from_blocking: libvsched2::TaskState,
    ) -> libvsched2::TaskState {
        self.task(|task| {
            task.match_set_state(
                state_from_ready,
                state_from_running,
                state_from_blocked,
                state_from_exited,
                state_from_blocking,
            )
        })
    }

    fn priority(&self) -> isize {
        self.task(libvsched2::Task::priority)
    }

    fn is_coroutine(&self) -> bool {
        self.task(libvsched2::Task::is_coroutine)
    }

    fn is_kernel(&self) -> bool {
        self.task(libvsched2::Task::is_kernel)
    }

    fn pid(&self) -> usize {
        self.task(libvsched2::Task::pid)
    }

    fn set_pid(&self, pid: usize) {
        self.task(|task| task.set_pid(pid));
    }

    fn resched(&self) {
        // resched 会非局部离开当前执行流，不能在持有 registry 锁时调用。
        self.direct().resched();
    }

    fn restore_context(&self) {
        // 恢复上下文后不会返回，当前任务的调度器所有权保证对象仍然有效。
        self.direct().restore_context();
    }

    fn poll(&self) -> Poll<isize> {
        // poll 内部可能主动让权，不能跨 poll 保持 registry 锁。
        self.direct().poll()
    }

    fn thread_stack(&self) -> *mut () {
        self.task(libvsched2::Task::thread_stack)
    }

    fn set_return_value(&self, value: isize) {
        self.task(|task| task.set_return_value(value));
    }

    fn dealloc(&self) {
        if let Some(key) = decode_task(self.raw()) {
            let task = take_user_task(key).expect("TaskImpl::dealloc: stale task id");
            unsafe { &*task }.dealloc_task(false);
        } else {
            self.task(libvsched2::Task::dealloc);
        }
    }
}

fn state_to_raw(state: libvsched2::TaskState) -> usize {
    match state {
        libvsched2::TaskState::Ready => SHARED_TASK_READY,
        libvsched2::TaskState::Running => SHARED_TASK_RUNNING,
        libvsched2::TaskState::Blocked => SHARED_TASK_BLOCKED,
        libvsched2::TaskState::Exited => SHARED_TASK_EXITED,
        libvsched2::TaskState::Blocking => SHARED_TASK_BLOCKING,
    }
}

fn state_from_raw(state: usize) -> libvsched2::TaskState {
    match state {
        SHARED_TASK_READY => libvsched2::TaskState::Ready,
        SHARED_TASK_RUNNING => libvsched2::TaskState::Running,
        SHARED_TASK_BLOCKED => libvsched2::TaskState::Blocked,
        SHARED_TASK_EXITED => libvsched2::TaskState::Exited,
        SHARED_TASK_BLOCKING => libvsched2::TaskState::Blocking,
        _ => panic!("invalid shared task state: {state}"),
    }
}

/// First entry for a normal kernel thread managed by vsched2.
///
/// `register_task` builds a frame that enters here on the task's axtask kernel
/// stack.  When the entry closure returns, commit Exited through the existing
/// vsched2 thread context-save path.
extern "C" fn kernel_thread_entry() -> ! {
    let current = libvsched2::current_task_ptr();
    with_vsched_task(current, |task| {
        axtask::run_task_entry_for_external_scheduler(&task.task);
        finish_kernel_thread(task, 0)
    })
    .expect("kernel_thread_entry: no vsched2 current task")
}

/// Completes an explicit `axtask::exit()` without entering AxRunQueue.
pub fn exit_current_kernel_thread(exit_code: i32) -> ! {
    let current = libvsched2::current_task_ptr();
    with_vsched_task(current, |task| {
        assert!(
            task.is_kernel.load(Ordering::Acquire),
            "axtask::exit may only exit the current vsched2 kernel task"
        );
        finish_kernel_thread(task, exit_code)
    })
    .expect("exit_current_kernel_thread: no vsched2 current task")
}

fn finish_kernel_thread(task: &VschedTaskImpl, exit_code: i32) -> ! {
    axhal::asm::disable_irqs();
    task.wake_generation.fetch_add(1, Ordering::AcqRel);
    axtask::notify_exit_for_external_scheduler(&task.task, exit_code);

    use libvsched2::Task as _;
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

/// 在限定作用域内解析 vsched2 任务。
pub fn with_vsched_task<R>(raw: *const (), f: impl FnOnce(&VschedTaskImpl) -> R) -> Option<R> {
    if let Some(key) = decode_task(raw) {
        return with_registered_user_task(key, f);
    }
    with_direct_task(raw, f)
}

/// 在限定作用域内解析直接任务指针。
fn with_direct_task<R>(raw: *const (), f: impl FnOnce(&VschedTaskImpl) -> R) -> Option<R> {
    let raw = core::ptr::NonNull::new(raw as *mut VschedTaskImpl)?;
    // SAFETY: 调用方传入的指针来自 vsched2 Task VTABLE；任务对象由 `register_task`
    // 分配，并且在从调度器注销前保持地址稳定。
    Some(unsafe { f(raw.as_ref()) })
}

/// 在限定作用域内访问当前 vsched2 任务。
pub fn with_current_vsched_task<R>(f: impl FnOnce(&VschedTaskImpl) -> R) -> Option<R> {
    with_vsched_task(libvsched2::current_task_ptr(), f)
}

/// 从 vsched2 返回的裸指针还原 AxTaskRef。
pub fn task_from_raw(task: *const ()) -> Option<AxTaskRef> {
    with_vsched_task(task, |vti| vti.task.clone())
}

/// 返回用户任务的内核对象地址。
pub fn direct_task(task: *const ()) -> Option<*const VschedTaskImpl> {
    if let Some(key) = decode_task(task) {
        let registry = USER_TASK_REGISTRY.lock();
        if !crate::vsched::shared_task_table().is_live(key) {
            return None;
        }
        return registry.get(&key).copied().map(|task| task as *const VschedTaskImpl);
    }
    (!task.is_null()).then_some(task as *const VschedTaskImpl)
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
        task, priority, pid, is_kernel, coroutine,
    ));
    vti.user_vdso_base.store(vdso_base, Ordering::Release);
    if is_thread {
        vti.thread_stack_ptr
            .store(VschedStackImpl::alloc() as usize, Ordering::Release);
    }
    if let Some(frame) = initial_frame {
        vti.trap_frame
            .store(Box::into_raw(frame) as usize, Ordering::Release);
    }
    Box::into_raw(vti)
}

/// 为用户任务分配共享槽，并登记到内核 registry。
///
/// 共享槽指针不会直接进入 `USER_SCHEDULER`；调度器保存编码后的
/// [`UserTaskKey`]，内核 [`TaskImpl`] 再通过 registry 将它解析为真实任务对象。
pub fn register_user_task(task: *const VschedTaskImpl, process_id: VschedProcessId) -> UserTaskKey {
    assert!(!task.is_null(), "register_user_task: null task");
    assert!(process_id.is_user(), "register_user_task: non-user process id");
    let task_impl = unsafe { &*task };
    assert!(
        !task_impl.is_kernel.load(Ordering::Acquire),
        "register_user_task: kernel task cannot use a shared user slot"
    );
    assert_eq!(
        task_impl.pid.load(Ordering::Acquire),
        process_id.as_raw(),
        "register_user_task: task/process scheduler id mismatch"
    );

    let key = crate::vsched::shared_task_table()
        .allocate(process_id, task_impl.priority.load(Ordering::Acquire))
        .expect("register_user_task: shared task table is full");
    let context_kind = if task_impl.is_coroutine.load(Ordering::Acquire) {
        SHARED_CONTEXT_COROUTINE
    } else {
        SHARED_CONTEXT_THREAD
    };
    assert!(
        crate::vsched::shared_task_table().initialize_context_kind(key, context_kind),
        "register_user_task: failed to publish initial context kind",
    );
    let initial_state = state_to_raw(to_vsched_state(task_impl.task.state()));
    crate::vsched::shared_task_table()
        .swap_task_state(key, initial_state)
        .expect("register_user_task: failed to publish initial task state");
    let mut registry = USER_TASK_REGISTRY.lock();
    if registry.insert(key, task as usize).is_some() {
        drop(registry);
        assert!(
            crate::vsched::shared_task_table().release(key),
            "register_user_task: failed to roll back duplicate key"
        );
        panic!("register_user_task: duplicate shared task key");
    }
    drop(registry);
    task_impl.install_shared_task_key(key);
    key
}

/// 移除用户任务的共享句柄；任务没有注册句柄时是幂等的。
pub fn unregister_user_task(task: *const VschedTaskImpl) {
    if task.is_null() {
        return;
    }
    let task_impl = unsafe { &*task };
    let Some(key) = task_impl.take_shared_task_key() else {
        return;
    };
    let mut registry = USER_TASK_REGISTRY.lock();
    let removed = registry.remove(&key);
    drop(registry);
    assert_eq!(
        removed,
        Some(task as usize),
        "unregister_user_task: registry owner mismatch"
    );
    assert!(
        crate::vsched::shared_task_table().release(key),
        "unregister_user_task: shared slot is already stale"
    );
}

/// 从 registry 中取出用户任务，并使任务 ID 失效。
fn take_user_task(key: UserTaskKey) -> Option<*const VschedTaskImpl> {
    let mut registry = USER_TASK_REGISTRY.lock();
    let task = *registry.get(&key)? as *const VschedTaskImpl;
    let task_impl = unsafe { &*task };
    if task_impl.take_shared_task_key() != Some(key) {
        return None;
    }
    registry.remove(&key);
    drop(registry);
    assert!(
        crate::vsched::shared_task_table().release(key),
        "take_user_task: shared task is stale",
    );
    Some(task)
}

/// 按稳定句柄查询内核任务引用。
///
/// 查询结果只返回 `AxTaskRef` 的受控强引用；共享槽中的用户可写字段不会被当作身份依据。
pub fn task_for_user_key(key: UserTaskKey) -> Option<AxTaskRef> {
    with_registered_user_task(key, |task| task.task.clone())
}

/// 在 registry 锁保护下访问共享句柄对应的内核任务。
///
/// 共享槽的用户可写投影不会参与解析；只有 registry 中仍登记的 `VschedTaskImpl` 才能
/// 被访问。回调不得再次调用任务注册/注销函数，否则会递归获取同一把 registry 锁。
pub fn with_registered_user_task<R>(
    key: UserTaskKey,
    f: impl FnOnce(&VschedTaskImpl) -> R,
) -> Option<R> {
    let registry = USER_TASK_REGISTRY.lock();
    // 保持 registry 锁直到回调结束，避免任务在解析后、使用前被 dealloc。
    let task = *registry.get(&key)?;
    if !crate::vsched::shared_task_table().is_live(key) {
        return None;
    }
    with_direct_task(task as *const (), f)
}
