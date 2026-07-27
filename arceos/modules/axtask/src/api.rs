//! Task APIs for multi-task configuration.

use alloc::{
    string::String,
    sync::{Arc, Weak},
};
use core::sync::atomic::AtomicUsize;

use kernel_guard::NoPreemptIrqSave;

/// Function pointer for vsched2 yield trampoline.
/// When set (non-zero), `yield_now()` delegates to vsched2 instead of AxRunQueue.
static VSCHED2_YIELD: AtomicUsize = AtomicUsize::new(0);
/// Context-conversion callback for the vsched2-aware `block_on` path.
/// `true` promotes the current coroutine to a thread before yield; `false`
/// restores a task promoted by the matching block_on invocation.
pub(crate) static BLOCK_ON_TOGGLE: AtomicUsize = AtomicUsize::new(0);

/// Atomically starts or cancels the task-state part of a block operation.
/// `true` commits Running -> Blocking; `false` cancels Blocking -> Running.
pub(crate) static BLOCK_ON_STATE: AtomicUsize = AtomicUsize::new(0);

/// Returns the externally scheduled task currently executing `block_on` and
/// its wait-generation.  The callback is installed by StarryOS' vsched2
/// adapter, so axtask does not need to depend on libvsched2.
pub(crate) static BLOCK_ON_CURRENT: AtomicUsize = AtomicUsize::new(0);

/// Wakes an externally scheduled task captured by `BLOCK_ON_CURRENT`.
pub(crate) static BLOCK_ON_WAKE: AtomicUsize = AtomicUsize::new(0);

/// Register the vsched2 yield trampoline. After this, all `yield_now()` calls
/// will enter the vsched2 scheduler instead of the legacy AxRunQueue.
pub fn register_vsched2_yield(yield_fn: unsafe extern "C" fn() -> !) {
    VSCHED2_YIELD.store(yield_fn as usize, core::sync::atomic::Ordering::Release);
}

/// Register the block_on context-conversion function.
pub fn register_block_on_toggle(toggle: fn(bool) -> bool) {
    BLOCK_ON_TOGGLE.store(toggle as usize, core::sync::atomic::Ordering::Release);
}

/// Registers the callbacks used by the vsched2-aware `block_on` backend.
///
/// The callbacks are deliberately function pointers instead of a dependency
/// on libvsched2: axtask is also used with the legacy AxRunQueue scheduler.
pub fn register_block_on_hooks(
    current: fn() -> (*const (), usize),
    wake: fn(*const (), usize) -> bool,
    state: fn(bool) -> bool,
) {
    BLOCK_ON_CURRENT.store(current as usize, core::sync::atomic::Ordering::Release);
    BLOCK_ON_WAKE.store(wake as usize, core::sync::atomic::Ordering::Release);
    BLOCK_ON_STATE.store(state as usize, core::sync::atomic::Ordering::Release);
}

pub(crate) fn block_on_current_task() -> Option<(*const (), usize)> {
    let ptr = BLOCK_ON_CURRENT.load(core::sync::atomic::Ordering::Acquire);
    if ptr == 0 {
        return None;
    }
    let current: fn() -> (*const (), usize) = unsafe { core::mem::transmute(ptr) };
    let (task, generation) = current();
    (!task.is_null()).then_some((task, generation))
}

pub(crate) fn wake_block_on_task(task: *const (), generation: usize) -> bool {
    let ptr = BLOCK_ON_WAKE.load(core::sync::atomic::Ordering::Acquire);
    if ptr == 0 || task.is_null() {
        return false;
    }
    let wake: fn(*const (), usize) -> bool = unsafe { core::mem::transmute(ptr) };
    wake(task, generation)
}

/// Commits (`blocking = true`) or cancels (`blocking = false`) the current
/// task's Blocking state.  The waiter's own atomic handshake decides when it
/// is safe for a remote CPU to call the wake callback.
pub(crate) fn transition_block_on_task(blocking: bool) -> Option<bool> {
    let ptr = BLOCK_ON_STATE.load(core::sync::atomic::Ordering::Acquire);
    if ptr == 0 {
        return None;
    }
    let state: fn(bool) -> bool = unsafe { core::mem::transmute(ptr) };
    Some(state(blocking))
}

/// Returns true if vsched2 yield is registered (non-zero).
pub fn vsched2_active() -> bool {
    VSCHED2_YIELD.load(core::sync::atomic::Ordering::Acquire) != 0
}

pub(crate) use crate::run_queue::{current_run_queue, select_run_queue};
#[doc(cfg(all(feature = "multitask", feature = "task-ext")))]
#[cfg(feature = "task-ext")]
pub use crate::task::{AxTaskExt, TaskExt};
#[doc(cfg(all(feature = "multitask", feature = "irq")))]
#[cfg(feature = "irq")]
pub use crate::timers::register_timer_callback;
#[doc(cfg(feature = "multitask"))]
pub use crate::{
    task::{CurrentTask, TaskId, TaskInner, TaskState},
    wait_queue::WaitQueue,
};

/// The reference type of a task.
pub type AxTaskRef = Arc<AxTask>;

/// The weak reference type of a task.
pub type WeakAxTaskRef = Weak<AxTask>;

/// The wrapper type for [`cpumask::CpuMask`] with SMP configuration.
pub type AxCpuMask = cpumask::CpuMask<{ axconfig::plat::CPU_NUM }>;

static CPU_NUM: AtomicUsize = AtomicUsize::new(1);

cfg_if::cfg_if! {
    if #[cfg(feature = "sched-rr")] {
        const MAX_TIME_SLICE: usize = 5;
        pub(crate) type AxTask = axsched::RRTask<TaskInner, MAX_TIME_SLICE>;
        pub(crate) type Scheduler = axsched::RRScheduler<TaskInner, MAX_TIME_SLICE>;
    } else if #[cfg(feature = "sched-cfs")] {
        pub(crate) type AxTask = axsched::CFSTask<TaskInner>;
        pub(crate) type Scheduler = axsched::CFScheduler<TaskInner>;
    } else {
        // If no scheduler features are set, use FIFO as the default.
        pub(crate) type AxTask = axsched::FifoTask<TaskInner>;
        pub(crate) type Scheduler = axsched::FifoScheduler<TaskInner>;
    }
}

#[cfg(feature = "preempt")]
struct KernelGuardIfImpl;

#[cfg(feature = "preempt")]
#[crate_interface::impl_interface]
impl kernel_guard::KernelGuardIf for KernelGuardIfImpl {
    fn disable_preempt() {
        if let Some(curr) = current_may_uninit() {
            curr.disable_preempt();
        }
    }

    fn enable_preempt() {
        if let Some(curr) = current_may_uninit() {
            curr.enable_preempt(true);
        }
    }
}

/// Gets the current task, or returns [`None`] if the current task is not
/// initialized.
pub fn current_may_uninit() -> Option<CurrentTask> {
    CurrentTask::try_get()
}

/// Gets the current task.
///
/// # Panics
///
/// Panics if the current task is not initialized.
pub fn current() -> CurrentTask {
    CurrentTask::get()
}

/// Initializes the task scheduler (for the primary CPU).
pub fn init_scheduler() {
    init_scheduler_with_cpu_num(axconfig::plat::CPU_NUM);
}

/// Under vsched2, AxRunQueue is unused but may still be accessed by
/// legacy code paths (AxWaker, timer tick, etc.). Initialize it as
/// an empty queue to prevent LazyInit panics. Safe to call even if
/// `init_scheduler()` already ran — `call_once` is idempotent.
pub fn init_run_queue_empty() {
    crate::run_queue::init_empty();
}

/// Initializes the task scheduler with cpu_num (for the primary CPU).
pub fn init_scheduler_with_cpu_num(cpu_num: usize) {
    info!("Initialize scheduling...");
    CPU_NUM.store(cpu_num, core::sync::atomic::Ordering::Relaxed);

    crate::run_queue::init();

    info!("  use {} scheduler.", Scheduler::scheduler_name());
}

pub(crate) fn active_cpu_num() -> usize {
    CPU_NUM.load(core::sync::atomic::Ordering::Relaxed)
}

/// Initializes the task scheduler for secondary CPUs.
pub fn init_scheduler_secondary() {
    crate::run_queue::init_secondary();
}

/// Handles periodic timer ticks for the task manager.
///
/// If vsched2 is active, timer is handled by vsched2's trap entry; skip
/// legacy AxRunQueue path entirely to avoid LazyInit panic.
#[cfg(feature = "irq")]
pub fn on_timer_tick() {
    if vsched2_active() {
        return;
    }
    crate::timers::check_events();
    current_run_queue::<kernel_guard::NoOp>().scheduler_timer_tick();
}

/// Adds the given task to the run queue, returns the task reference.
pub fn spawn_task(task: TaskInner) -> AxTaskRef {
    let task_ref = task.into_arc();
    select_run_queue::<NoPreemptIrqSave>(&task_ref).add_task(task_ref.clone());
    task_ref
}

/// Spawns a new task with the given parameters.
///
/// Returns the task reference.
pub fn spawn_raw<F>(f: F, name: String, stack_size: usize) -> AxTaskRef
where
    F: FnOnce() + Send + 'static,
{
    spawn_task(TaskInner::new(f, name, stack_size))
}

/// Creates a new task without adding it to the run queue.
///
/// Useful when the task will be managed by an external scheduler (e.g. vsched2).
pub fn new_raw<F>(f: F, name: String, stack_size: usize) -> AxTaskRef
where
    F: FnOnce() + Send + 'static,
{
    TaskInner::new(f, name, stack_size).into_arc()
}

/// Wraps an existing TaskInner into AxTaskRef without adding it to the run queue.
pub fn into_ref(task: TaskInner) -> AxTaskRef {
    task.into_arc()
}

/// Spawns a new task with the given name and the default stack size ([`axconfig::TASK_STACK_SIZE`]).
///
/// Returns the task reference.
pub fn spawn_with_name<F>(f: F, name: String) -> AxTaskRef
where
    F: FnOnce() + Send + 'static,
{
    spawn_raw(f, name, axconfig::TASK_STACK_SIZE)
}

/// Spawns a new task with the default parameters.
///
/// The default task name is an empty string. The default task stack size is
/// [`axconfig::TASK_STACK_SIZE`].
///
/// Returns the task reference.
pub fn spawn<F>(f: F) -> AxTaskRef
where
    F: FnOnce() + Send + 'static,
{
    spawn_with_name(f, String::new())
}

/// Set the priority for current task.
///
/// The range of the priority is dependent on the underlying scheduler. For
/// example, in the [CFS] scheduler, the priority is the nice value, ranging from
/// -20 to 19.
///
/// Returns `true` if the priority is set successfully.
///
/// [CFS]: https://en.wikipedia.org/wiki/Completely_Fair_Scheduler
pub fn set_priority(prio: isize) -> bool {
    current_run_queue::<NoPreemptIrqSave>().set_current_priority(prio)
}

/// Temporarily override the current task for the duration of `f`.
///
/// All calls to [`current()`] inside `f` will return `task`.
/// The original current task is restored after `f` returns.
///
/// This is useful when running code that assumes a specific task is
/// "current" — for example, syscall handlers that call [`current()`]
/// to access process data.
pub fn with_current_task<R>(task: &AxTaskRef, f: impl FnOnce() -> R) -> R {
    let old_ptr = axhal::percpu::current_task_ptr::<super::AxTask>();
    assert!(!old_ptr.is_null(), "with_current_task: no current task");

    let new_ptr = Arc::into_raw(task.clone());
    unsafe { axhal::percpu::set_current_task_ptr(new_ptr) };

    let result = f();

    let current_new = axhal::percpu::current_task_ptr::<super::AxTask>();
    debug_assert_eq!(current_new, new_ptr, "with_current_task: current changed during f");
    unsafe { Arc::from_raw(current_new) };
    unsafe { axhal::percpu::set_current_task_ptr(old_ptr) };

    result
}

/// Installs `task` as the axtask current task without using `AxRunQueue`.
///
/// External schedulers must call this whenever they restore a kernel thread,
/// otherwise two externally scheduled threads can observe the same stale
/// per-CPU `axtask::current()` value.
pub fn install_current_task_for_external_scheduler(task: &AxTaskRef) {
    let old_ptr = axhal::percpu::current_task_ptr::<super::AxTask>();
    let target_ptr = Arc::as_ptr(task);

    if old_ptr != target_ptr {
        let new_ptr = Arc::into_raw(task.clone());
        unsafe { axhal::percpu::set_current_task_ptr(new_ptr) };
        if !old_ptr.is_null() {
            // SAFETY: the per-CPU current-task pointer owns one strong Arc
            // reference, established by init_current/set_current or this bridge.
            unsafe { drop(Arc::from_raw(old_ptr)) };
        }
    }
}

/// Runs the entry closure of an externally scheduled task.
///
/// The caller must already have switched to the task's kernel stack.  The
/// closure is allowed to return; committing the external scheduler's exit
/// state remains the caller's responsibility.
pub fn run_task_entry_for_external_scheduler(task: &AxTaskRef) {
    install_current_task_for_external_scheduler(task);

    task.run_entry_for_external_scheduler();
}

/// Set the affinity for the current task.
/// [`AxCpuMask`] is used to specify the CPU affinity.
/// Returns `true` if the affinity is set successfully.
///
/// TODO: support set the affinity for other tasks.
pub fn set_current_affinity(cpumask: AxCpuMask) -> bool {
    if cpumask.is_empty() {
        false
    } else {
        let curr = current().clone();

        curr.set_cpumask(cpumask);
        // After setting the affinity, we need to check if current cpu matches
        // the affinity. If not, we need to migrate the task to the correct CPU.
        #[cfg(feature = "smp")]
        if !cpumask.get(axhal::percpu::this_cpu_id()) {
            const MIGRATION_TASK_STACK_SIZE: usize = 4096;
            // Spawn a new migration task for migrating.
            let migration_task = TaskInner::new(
                move || crate::run_queue::migrate_entry(curr),
                "migration-task".into(),
                MIGRATION_TASK_STACK_SIZE,
            )
            .into_arc();

            // Migrate the current task to the correct CPU using the migration task.
            current_run_queue::<NoPreemptIrqSave>().migrate_current(migration_task);

            assert!(
                cpumask.get(axhal::percpu::this_cpu_id()),
                "Migration failed"
            );
        }
        true
    }
}

/// Current task gives up the CPU time voluntarily, and switches to another
/// ready task.
///
/// If vsched2 yield is registered (non-zero), delegates to vsched2's yield
/// trampoline instead of the legacy AxRunQueue.
pub fn yield_now() {
    let f = VSCHED2_YIELD.load(core::sync::atomic::Ordering::Acquire);
    if f != 0 {
        unsafe {
            core::arch::asm!("jalr {f}", f = in(reg) f);
        }
        return;
    }
    current_run_queue::<NoPreemptIrqSave>().yield_current()
}

/// Current task is going to sleep for the given duration.
///
/// If the feature `irq` is not enabled, it uses busy-wait instead.
pub fn sleep(dur: core::time::Duration) {
    sleep_until(axhal::time::wall_time() + dur);
}

/// Current task is going to sleep, it will be woken up at the given deadline.
///
/// If the feature `irq` is not enabled, it uses busy-wait instead.
pub fn sleep_until(deadline: axhal::time::TimeValue) {
    #[cfg(feature = "irq")]
    crate::future::block_on(crate::future::sleep_until(deadline));
    #[cfg(not(feature = "irq"))]
    axhal::time::busy_wait_until(deadline);
}

/// Exits the current task.
pub fn exit(exit_code: i32) -> ! {
    current_run_queue::<NoPreemptIrqSave>().exit_current(exit_code)
}

/// The idle task routine.
///
/// It runs an infinite loop that keeps calling [`yield_now()`].
pub fn run_idle() -> ! {
    loop {
        yield_now();
        trace!("idle task: waiting for IRQs...");
        #[cfg(feature = "irq")]
        axhal::asm::wait_for_irqs();
    }
}
