#[cfg(feature = "thread")]
use crate::TaskStack;
use crate::{stat::TimeStat, Scheduler, TrapFrame};
use alloc::{boxed::Box, collections::vec_deque::VecDeque, string::String, sync::Arc};
use core::{
    cell::UnsafeCell,
    fmt,
    future::Future,
    mem::ManuallyDrop,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering},
    task::Waker,
};
use kspin::{SpinNoIrq, SpinNoIrqGuard};

/// A unique identifier for a thread.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TaskId(usize);

static ID_COUNTER: AtomicUsize = AtomicUsize::new(1);
impl TaskId {
    /// Create a new task ID.
    pub fn new() -> Self {
        Self(ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    pub const fn as_usize(&self) -> usize {
        self.0 as usize
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

/// The possible states of a task.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum TaskState {
    Running = 0,
    Runable = 1,
    Blocking = 2,
    Waked = 3,
    Blocked = 4,
    Exited = 5,
}

#[derive(PartialEq, Eq, Clone, Copy)]
#[allow(non_camel_case_types)]
/// The policy of the scheduler
pub enum SchedPolicy {
    /// The default time-sharing scheduler
    SCHED_OTHER = 0,
    /// The first-in, first-out scheduler
    SCHED_FIFO = 1,
    /// The round-robin scheduler
    SCHED_RR = 2,
    /// The batch scheduler
    SCHED_BATCH = 3,
    /// The idle task scheduler
    SCHED_IDLE = 5,
    /// Unknown scheduler
    SCHED_UNKNOWN,
}

impl From<usize> for SchedPolicy {
    #[inline]
    fn from(policy: usize) -> Self {
        match policy {
            0 => SchedPolicy::SCHED_OTHER,
            1 => SchedPolicy::SCHED_FIFO,
            2 => SchedPolicy::SCHED_RR,
            3 => SchedPolicy::SCHED_BATCH,
            5 => SchedPolicy::SCHED_IDLE,
            _ => SchedPolicy::SCHED_UNKNOWN,
        }
    }
}

impl From<SchedPolicy> for isize {
    #[inline]
    fn from(policy: SchedPolicy) -> Self {
        match policy {
            SchedPolicy::SCHED_OTHER => 0,
            SchedPolicy::SCHED_FIFO => 1,
            SchedPolicy::SCHED_RR => 2,
            SchedPolicy::SCHED_BATCH => 3,
            SchedPolicy::SCHED_IDLE => 5,
            SchedPolicy::SCHED_UNKNOWN => -1,
        }
    }
}

#[derive(Clone, Copy)]
/// The status of the scheduler
pub struct SchedStatus {
    /// The policy of the scheduler
    pub policy: SchedPolicy,
    /// The priority of the scheduler policy
    pub priority: usize,
}

pub struct TaskInner {
    fut: UnsafeCell<Pin<Box<dyn Future<Output = isize> + 'static>>>,
    utrap_frame: UnsafeCell<Option<Box<TrapFrame>>>,

    // executor: SpinNoIrq<Arc<Executor>>,
    pub(crate) wait_wakers: UnsafeCell<VecDeque<Waker>>,
    pub(crate) scheduler: SpinNoIrq<Arc<SpinNoIrq<Scheduler>>>,

    pub(crate) id: TaskId,
    pub(crate) name: UnsafeCell<String>,
    /// Whether the task is the initial task
    ///
    /// If the task is the initial task, the kernel will terminate
    /// when the task exits.
    pub(crate) is_init: bool,
    pub(crate) state: SpinNoIrq<TaskState>,
    time: UnsafeCell<TimeStat>,
    exit_code: AtomicIsize,
    set_child_tid: AtomicUsize,
    clear_child_tid: AtomicUsize,
    #[cfg(feature = "preempt")]
    /// Whether the task needs to be rescheduled
    ///
    /// When the time slice is exhausted, it needs to be rescheduled
    need_resched: AtomicBool,
    #[cfg(feature = "preempt")]
    /// The disable count of preemption
    ///
    /// When the task get a lock which need to disable preemption, it
    /// will increase the count. When the lock is released, it will
    /// decrease the count.
    ///
    /// Only when the count is zero, the task can be preempted.
    preempt_disable_count: AtomicUsize,
    /// 在内核中发生抢占或者使用线程接口时的上下文
    #[cfg(feature = "thread")]
    stack_ctx: UnsafeCell<Option<StackCtx>>,

    /// 是否是所属进程下的主线程
    is_leader: AtomicBool,
    process_id: AtomicUsize,
    pub page_table_token: UnsafeCell<usize>,

    /// The scheduler status of the task, which defines the scheduling policy and priority
    pub sched_status: UnsafeCell<SchedStatus>,
    pub cpu_set: AtomicUsize,
}

unsafe impl Send for TaskInner {}
unsafe impl Sync for TaskInner {}

impl TaskInner {
    pub fn new(
        name: String,
        process_id: usize,
        scheduler: Arc<SpinNoIrq<Scheduler>>,
        page_table_token: usize,
        fut: Pin<Box<dyn Future<Output = isize> + 'static>>,
    ) -> Self {
        let is_init = &name == "main";
        let t = Self {
            id: TaskId::new(),
            name: UnsafeCell::new(name),
            is_init,
            exit_code: AtomicIsize::new(0),
            fut: UnsafeCell::new(fut),
            utrap_frame: UnsafeCell::new(None),
            wait_wakers: UnsafeCell::new(VecDeque::new()),
            scheduler: SpinNoIrq::new(scheduler),
            state: SpinNoIrq::new(TaskState::Runable),
            time: UnsafeCell::new(TimeStat::new()),
            set_child_tid: AtomicUsize::new(0),
            clear_child_tid: AtomicUsize::new(0),
            #[cfg(feature = "preempt")]
            need_resched: AtomicBool::new(false),
            #[cfg(feature = "preempt")]
            preempt_disable_count: AtomicUsize::new(0),
            is_leader: AtomicBool::new(false),
            process_id: AtomicUsize::new(process_id),
            page_table_token: UnsafeCell::new(page_table_token),
            #[cfg(feature = "thread")]
            stack_ctx: UnsafeCell::new(None),
            sched_status: UnsafeCell::new(SchedStatus {
                policy: SchedPolicy::SCHED_FIFO,
                priority: 1,
            }),
            cpu_set: AtomicUsize::new(0),
        };
        t.set_cpu_set((1 << axconfig::plat::CPU_NUM) - 1, 1, axconfig::plat::CPU_NUM);
        t
    }

    pub fn new_user(
        name: String,
        process_id: usize,
        scheduler: Arc<SpinNoIrq<Scheduler>>,
        page_table_token: usize,
        fut: Pin<Box<dyn Future<Output = isize> + 'static>>,
        utrap_frame: Box<TrapFrame>,
    ) -> Self {
        let is_init = &name == "main";
        let t = Self {
            id: TaskId::new(),
            name: UnsafeCell::new(name),
            is_init,
            exit_code: AtomicIsize::new(0),
            fut: UnsafeCell::new(fut),
            utrap_frame: UnsafeCell::new(Some(utrap_frame)),
            wait_wakers: UnsafeCell::new(VecDeque::new()),
            scheduler: SpinNoIrq::new(scheduler),
            state: SpinNoIrq::new(TaskState::Runable),
            time: UnsafeCell::new(TimeStat::new()),
            set_child_tid: AtomicUsize::new(0),
            clear_child_tid: AtomicUsize::new(0),
            #[cfg(feature = "preempt")]
            need_resched: AtomicBool::new(false),
            #[cfg(feature = "preempt")]
            preempt_disable_count: AtomicUsize::new(0),
            is_leader: AtomicBool::new(false),
            process_id: AtomicUsize::new(process_id),
            page_table_token: UnsafeCell::new(page_table_token),
            #[cfg(feature = "thread")]
            stack_ctx: UnsafeCell::new(None),
            sched_status: UnsafeCell::new(SchedStatus {
                policy: SchedPolicy::SCHED_FIFO,
                priority: 1,
            }),
            cpu_set: AtomicUsize::new(0),
        };
        t.set_cpu_set((1 << axconfig::plat::CPU_NUM) - 1, 1, axconfig::plat::CPU_NUM);
        t
    }

    /// 获取到任务的 Future
    pub fn get_fut(&self) -> &mut Pin<Box<dyn Future<Output = isize> + 'static>> {
        unsafe { &mut *self.fut.get() }
    }

    /// Gets the ID of the task.
    pub const fn id(&self) -> TaskId {
        self.id
    }

    /// Gets the name of the task.
    pub fn name(&self) -> &str {
        unsafe { (*self.name.get()).as_str() }
    }

    /// Sets the name of the task.
    pub fn set_name(&self, name: &str) {
        unsafe {
            *self.name.get() = String::from(name);
        }
    }

    /// Get a combined string of the task ID and name.
    pub fn id_name(&self) -> alloc::string::String {
        alloc::format!("Task({}, {:?})", self.id.as_usize(), self.name())
    }

    /// Whether the task has been inited
    #[inline]
    pub const fn is_init(&self) -> bool {
        self.is_init
    }

    /// set the scheduling policy and priority
    pub fn set_sched_status(&self, status: SchedStatus) {
        let prev_status = self.sched_status.get();
        unsafe {
            *prev_status = status;
        }
    }

    /// get the scheduling policy and priority
    pub fn get_sched_status(&self) -> SchedStatus {
        let status = self.sched_status.get();
        unsafe { *status }
    }

    /// 设置CPU set，其中set_size为bytes长度
    pub fn set_cpu_set(&self, mask: usize, set_size: usize, max_cpu_num: usize) {
        let len = if set_size * 4 > max_cpu_num {
            max_cpu_num
        } else {
            set_size * 4
        };
        let now_mask = mask & 1 << ((len) - 1);
        self.cpu_set.store(now_mask, Ordering::Release)
    }

    /// to get the CPU set
    pub fn get_cpu_set(&self) -> usize {
        self.cpu_set.load(Ordering::Acquire) as usize
    }

    /// Get the exit code
    #[inline]
    pub fn get_exit_code(&self) -> isize {
        self.exit_code.load(Ordering::Acquire)
    }

    /// Set the task exit code
    #[inline]
    pub fn set_exit_code(&self, code: isize) {
        self.exit_code.store(code, Ordering::Release)
    }

    #[inline]
    /// set the state of the task
    pub fn state(&self) -> TaskState {
        *self.state.lock()
    }

    #[inline]
    /// lock the task state and ctx_ptr access
    pub fn state_lock_manual(&'_ self) -> ManuallyDrop<SpinNoIrqGuard<'_, TaskState>> {
        ManuallyDrop::new(self.state.lock())
    }

    #[inline]
    /// set the state of the task
    pub fn set_state(&self, state: TaskState) {
        *self.state.lock() = state
    }

    /// Whether the task is Exited
    #[inline]
    pub fn is_exited(&self) -> bool {
        matches!(*self.state.lock(), TaskState::Exited)
    }

    /// Whether the task is runnalbe
    #[inline]
    pub fn is_runable(&self) -> bool {
        matches!(*self.state.lock(), TaskState::Runable)
    }

    /// Whether the task is blocking
    #[inline]
    pub fn is_blocking(&self) -> bool {
        matches!(*self.state.lock(), TaskState::Blocking)
    }

    /// Whether the task is blocked
    #[inline]
    pub fn is_blocked(&self) -> bool {
        matches!(*self.state.lock(), TaskState::Blocked)
    }

    pub fn get_scheduler(&self) -> Arc<SpinNoIrq<Scheduler>> {
        self.scheduler.lock().clone()
    }

    pub fn set_scheduler(&self, scheduler: Arc<SpinNoIrq<Scheduler>>) {
        *self.scheduler.lock() = scheduler;
    }

    /// store the child thread ID at the location pointed to by child_tid in clone args
    pub fn set_child_tid(&self, tid: usize) {
        self.set_child_tid.store(tid, Ordering::Release)
    }

    /// clear (zero) the child thread ID at the location pointed to by child_tid in clone args
    pub fn set_clear_child_tid(&self, tid: usize) {
        self.clear_child_tid.store(tid, Ordering::Release)
    }

    /// get the pointer to the child thread ID
    pub fn get_clear_child_tid(&self) -> usize {
        self.clear_child_tid.load(Ordering::Acquire) as usize
    }

    /// set the flag whether the task is the main thread of the process
    pub fn set_leader(&self, is_lead: bool) {
        self.is_leader.store(is_lead, Ordering::Release);
    }

    /// whether the task is the main thread of the process
    pub fn is_leader(&self) -> bool {
        self.is_leader.load(Ordering::Acquire)
    }

    #[inline]
    /// get the process ID of the task
    pub fn get_process_id(&self) -> usize {
        self.process_id.load(Ordering::Acquire)
    }

    #[inline]
    /// set the process ID of the task
    pub fn set_process_id(&self, process_id: usize) {
        self.process_id.store(process_id, Ordering::Release);
    }

    #[inline]
    /// get the page table token of the process which the task belongs to
    pub fn get_page_table_token(&self) -> usize {
        unsafe { *self.page_table_token.get() }
    }

    #[inline]
    /// force to set the page table token of the process UNSAFELY
    pub fn set_page_table_token(&self, token: usize) {
        unsafe {
            *self.page_table_token.get() = token;
        }
    }
}

/// Methods for task switch
impl TaskInner {
    pub fn notify_waker_for_exit(&self) {
        let wait_wakers = unsafe { &mut *self.wait_wakers.get() };
        while let Some(waker) = wait_wakers.pop_front() {
            waker.wake();
        }
    }

    pub fn join(&self, waker: Waker) {
        let task = waker.data() as *const crate::Task;
        unsafe { &*task }.set_state(TaskState::Blocking);
        let wait_wakers = unsafe { &mut *self.wait_wakers.get() };
        wait_wakers.push_back(waker);
    }

    pub fn utrap_frame(&self) -> Option<&mut TrapFrame> {
        unsafe { &mut *self.utrap_frame.get() }
            .as_mut()
            .map(|tf| tf.as_mut())
    }
}

/// Methods for time statistics
impl TaskInner {
    #[inline]
    /// update the time information when the task is switched from user mode to kernel mode
    pub fn time_stat_from_user_to_kernel(&self, current_tick: usize) {
        let time = self.time.get();
        unsafe {
            (*time).switch_into_kernel_mode(self.id.as_usize() as isize, current_tick);
        }
    }

    #[inline]
    /// update the time information when the task is switched from kernel mode to user mode
    pub fn time_stat_from_kernel_to_user(&self, current_tick: usize) {
        let time = self.time.get();
        unsafe {
            (*time).switch_into_user_mode(self.id.as_usize() as isize, current_tick);
        }
    }

    #[inline]
    /// update the time information when the task is switched out
    pub fn time_stat_when_switch_from(&self, current_tick: usize) {
        let time = self.time.get();
        unsafe {
            (*time).swtich_from_old_task(self.id.as_usize() as isize, current_tick);
        }
    }

    #[inline]
    /// update the time information when the task is ready to be switched in
    pub fn time_stat_when_switch_to(&self, current_tick: usize) {
        let time = self.time.get();
        unsafe {
            (*time).switch_to_new_task(self.id.as_usize() as isize, current_tick);
        }
    }

    #[inline]
    /// output the time statistics
    ///
    /// The format is (user time, kernel time) in nanoseconds
    pub fn time_stat_output(&self) -> (usize, usize) {
        let time = self.time.get();
        unsafe { (*time).output() }
    }

    #[inline]
    /// 输出计时器信息
    /// (计时器周期，当前计时器剩余时间)
    /// 单位为us
    pub fn timer_output(&self) -> (usize, usize) {
        let time = self.time.get();
        unsafe { (*time).output_timer_as_us() }
    }

    #[inline]
    /// 设置计时器信息
    ///
    /// 若type不为None则返回成功
    pub fn set_timer(
        &self,
        timer_interval_ns: usize,
        timer_remained_ns: usize,
        timer_type: usize,
    ) -> bool {
        let time = self.time.get();
        unsafe { (*time).set_timer(timer_interval_ns, timer_remained_ns, timer_type) }
    }

    #[inline]
    /// 重置统计时间
    pub fn time_stat_reset(&self, current_tick: usize) {
        let time = self.time.get();
        unsafe {
            (*time).reset(current_tick);
        }
    }

    /// Check whether the timer triggered
    ///
    /// If the timer has triggered, then reset it and return the signal number
    pub fn check_pending_signal(&self) -> Option<usize> {
        let time = self.time.get();
        unsafe { (*time).check_pending_timer_signal() }
    }
}

#[cfg(feature = "preempt")]
impl TaskInner {
    /// Set the task waiting for reschedule
    #[inline]
    pub fn set_preempt_pending(&self, pending: bool) {
        self.need_resched.store(pending, Ordering::Release)
    }

    /// Get whether the task is waiting for reschedule
    #[inline]
    pub fn get_preempt_pending(&self) -> bool {
        self.need_resched.load(Ordering::Acquire)
    }

    /// Whether the task can be preempted
    #[inline]
    pub fn can_preempt(&self) -> bool {
        self.preempt_disable_count.load(Ordering::Acquire) == 0
    }

    /// Disable the preemption
    #[inline]
    pub fn disable_preempt(&self) {
        self.preempt_disable_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Enable the preemption by increasing the disable count
    ///
    /// Only when the count is zero, the task can be preempted
    #[inline]
    pub fn enable_preempt(&self) {
        self.preempt_disable_count.fetch_sub(1, Ordering::Relaxed);
    }

    /// Get the number of preempt disable counter
    #[inline]
    pub fn preempt_num(&self) -> usize {
        self.preempt_disable_count.load(Ordering::Acquire)
    }
}

impl fmt::Debug for TaskInner {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("TaskInner")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish()
    }
}

impl Drop for TaskInner {
    fn drop(&mut self) {
        log::debug!("task drop: {}", self.id_name());
    }
}

#[cfg(feature = "thread")]
#[repr(usize)]
pub enum CtxType {
    /// 其中的 usize 是中断状态，在使用线程接口让权时，将当前的中断状态保存至此，并且关闭中断
    /// 在线程恢复执行后，需要恢复原来的中断状态
    Thread = 0,
    #[cfg(feature = "preempt")]
    Interrupt,
}

#[cfg(feature = "thread")]
pub struct StackCtx {
    pub kstack: TaskStack,
    pub trap_frame: *const TrapFrame,
    pub ctx_type: CtxType,
}

#[cfg(feature = "thread")]
/// 线程的接口需要根据任务的状态来进行不同的操作
impl TaskInner {
    pub fn set_stack_ctx(&self, trap_frame: *const TrapFrame, ctx_type: CtxType) {
        let stack_ctx = unsafe { &mut *self.stack_ctx.get() };
        assert!(
            stack_ctx.is_none(),
            "{} cannot use thread api to do task switch",
            self.id_name()
        );
        let kstack = crate::pick_current_stack();
        stack_ctx.replace(StackCtx {
            kstack,
            trap_frame,
            ctx_type,
        });
    }

    pub fn get_stack_ctx(&self) -> Option<StackCtx> {
        let stack_ctx = unsafe { &mut *self.stack_ctx.get() };
        stack_ctx.take()
    }
}
