use alloc::{string::String, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axhal::asm;
use axmm;
use axtask::TaskState as AxTaskState;
use libvsched2::{self, Stack as _};
use vdso;

pub mod context;
pub(crate) mod smp;
pub mod stack;
pub mod task;
pub mod trap;
mod trap_vector;
pub mod trapframe;
mod userdata;

pub use task::{CoroutinePoll, VschedTaskImpl, register_task, task_from_raw};

use crate::config;

pub static mut VSCHED2_VVAR_START_PA: usize = 0;
pub static mut VSCHED2_VVAR_SIZE: usize = 0;
pub static mut VSCHED2_VDSO_START_PA: usize = 0;
pub static mut VSCHED2_VDSO_SIZE: usize = 0;

static VSCHED2_READY: AtomicBool = AtomicBool::new(false);
/// `starry_api::init()` may create background kernel threads before the
/// vsched2 scheduler itself is initialized.  This flag selects the external
/// scheduler path early without making those threads visible to AxRunQueue.
static VSCHED2_PREPARED: AtomicBool = AtomicBool::new(false);
/// Becomes true only after `kernel_init_main` has initialized the kernel
/// scheduler.  Before this point external-scheduler threads are kept pending.
static VSCHED2_SCHEDULER_READY: AtomicBool = AtomicBool::new(false);
/// The initial userspace task passed to `vsched2_bootstrap`.
///
/// Unlike the legacy path, `vsched2_bootstrap` never returns to `main`, so the
/// init task cannot be joined there before powering off.  Keep its scheduler
/// identity here so only that task's exit terminates the whole system.
static VSCHED2_INIT_TASK: AtomicUsize = AtomicUsize::new(0);

struct PendingKernelThread {
    task: axtask::AxTaskRef,
    priority: isize,
}

static PENDING_KERNEL_THREADS: spin::Mutex<Vec<PendingKernelThread>> =
    spin::Mutex::new(Vec::new());

/// 内核 SATP 值，在 activate_vsched_trap_vector 时写入, trap 向量直接加载
pub static KERNEL_SATP_VAL: AtomicUsize = AtomicUsize::new(0);
/// 内核 gp 值, activate_vsched_trap_vector 时记录, trap 向量恢复
pub static KERNEL_GP: AtomicUsize = AtomicUsize::new(0);
/// trap 向量暂存用户 t0 (避免用用户栈)
pub static TRAP_SCRATCH: AtomicUsize = AtomicUsize::new(0);
/// 最近一次进入用户态前保存的任务指针
pub static LAST_USER_TASK: AtomicUsize = AtomicUsize::new(0);
/// 当前活跃用户页表根, 用于陷阱向量进入 VDSO 前切换页表
pub static LAST_USER_PT_ROOT: AtomicUsize = AtomicUsize::new(0);
pub static KERNEL_VVAR_BASE: AtomicUsize = AtomicUsize::new(0);
pub static KERNEL_KSCHEDULER: AtomicUsize = AtomicUsize::new(0);
/// 当前正在被 trap_handler 服务的用户任务（vsched2 指针）。
/// dispatch 时写入，mark_exited 时读取。不被 vsched2 CURRENT_TASK 覆盖。
/// per-CPU 数组，每核心独立。
const CPU_NUM: usize = axconfig::plat::CPU_NUM;
pub static TRAPPED_VSCHED_TASK: [AtomicUsize; CPU_NUM] = [const { AtomicUsize::new(0) }; CPU_NUM];

pub const HIGHEST_PRIORITY: isize = 0;
pub const LOWEST_PRIORITY: isize = 15;
/// KERNEL_STACK_SIZE for trap vector assembly offset adjustment
pub const KERNEL_STACK: usize = config::KERNEL_STACK_SIZE;

pub fn to_vsched_state(s: AxTaskState) -> libvsched2::TaskState {
    match s {
        AxTaskState::Ready => libvsched2::TaskState::Ready,
        AxTaskState::Running => libvsched2::TaskState::Running,
        AxTaskState::Blocked => libvsched2::TaskState::Blocked,
        AxTaskState::Exited => libvsched2::TaskState::Exited,
        AxTaskState::Blocking => libvsched2::TaskState::Blocking,
    }
}

pub fn from_vsched_state(s: libvsched2::TaskState) -> AxTaskState {
    match s {
        libvsched2::TaskState::Ready => AxTaskState::Ready,
        libvsched2::TaskState::Running => AxTaskState::Running,
        libvsched2::TaskState::Blocked => AxTaskState::Blocked,
        libvsched2::TaskState::Blocking => AxTaskState::Blocking,
        libvsched2::TaskState::Exited => AxTaskState::Exited,
    }
}

pub fn init_vsched2_interfaces() {
    if VSCHED2_READY.swap(true, Ordering::AcqRel) {
        return;
    }

    unsafe {
        VSCHED2_VVAR_START_PA = vdso::VVAR_START_PA;
        VSCHED2_VVAR_SIZE = vdso::VVAR_SIZE;
        VSCHED2_VDSO_START_PA = vdso::VDSO_START_PA;
        VSCHED2_VDSO_SIZE = vdso::VDSO_SIZE;
    }

    libvsched2::init_vtable_Task::<VschedTaskImpl>();
    libvsched2::init_vtable_Stack::<stack::VschedStackImpl>();
    libvsched2::init_vtable_Context::<context::VschedContextImpl>();
    libvsched2::init_vtable_TrapInfo::<trap::VschedTrapInfoImpl>();
    libvsched2::init_vtable_SMP::<smp::VschedSmpImpl>();
    libvsched2::init_vtable_VSpace::<context::VschedVSpaceImpl>();
    libvsched2::init_vtable_UserData::<userdata::VschedUserDataImpl>();
    context::init_raw_run_task_offset();
}

unsafe extern "C" {
    fn vsched2_trap_vector();
}

pub fn activate_vsched_trap_vector() {
    let kernel_satp: usize;
    unsafe { core::arch::asm!("csrr {}, satp", out(reg) kernel_satp) };
    KERNEL_SATP_VAL.store(kernel_satp, Ordering::Release);

    let kernel_gp: usize;
    unsafe { core::arch::asm!("mv {}, gp", out(reg) kernel_gp) };
    KERNEL_GP.store(kernel_gp, Ordering::Release);

    unsafe {
        axhal::asm::write_trap_vector_base(vsched2_trap_vector as *const () as usize);
    }
}

pub fn push_task_to_kernel(task_ptr: *const ()) -> bool {
    // `push_task` selects the queue from Task::is_kernel().  Unlike
    // `push_task_into_current`, this remains correct when a kernel service
    // spawns a task while handling a user process on its behalf.
    libvsched2::push_task(task_ptr)
}

/// Selects vsched2 before `starry_api::init()` creates background tasks.
///
/// The scheduler is not usable yet, so tasks created in this interval are
/// retained by `PENDING_KERNEL_THREADS` and registered during bootstrap.
pub fn prepare_vsched2() {
    if VSCHED2_PREPARED.load(Ordering::Acquire) {
        return;
    }
    axtask::register_external_scheduler_hooks(
        external_spawn_kernel_thread,
        task::exit_current_kernel_thread,
        set_external_task_priority,
        reject_external_task_affinity,
    );
    VSCHED2_PREPARED.store(true, Ordering::Release);
}

fn external_spawn_kernel_thread(task: axtask::AxTaskRef) {
    enqueue_kernel_thread(task, HIGHEST_PRIORITY + 1);
}

fn set_external_task_priority(priority: isize) -> bool {
    if !(HIGHEST_PRIORITY..=LOWEST_PRIORITY).contains(&priority) {
        return false;
    }
    let current = current_task_ptr() as *const task::VschedTaskImpl;
    if current.is_null() {
        return false;
    }
    let current = unsafe { &*current };
    if !current.is_kernel.load(Ordering::Acquire) {
        return false;
    }
    current.priority.store(priority, Ordering::Release);
    true
}

fn reject_external_task_affinity(_cpumask: axtask::AxCpuMask) -> bool {
    // vsched2 does not yet expose an affinity/migration operation.  Returning
    // failure is preferable to updating only AxTask metadata or entering the
    // legacy AxRunQueue migration path, both of which would claim semantics
    // that the active scheduler cannot enforce.
    false
}

fn register_kernel_thread(task: axtask::AxTaskRef, priority: isize) {
    let task_ptr = register_task(task, priority, 0, true, None, 0);
    assert!(
        push_task_to_kernel(task_ptr as *const ()),
        "vsched2 kernel ready queue is full"
    );
}

fn enqueue_kernel_thread(task: axtask::AxTaskRef, priority: isize) {
    axlog::info!(
        "[vsched2] kernel task accepted: name={} priority={}",
        task.name(),
        priority
    );
    let mut pending = PENDING_KERNEL_THREADS.lock();
    if !VSCHED2_SCHEDULER_READY.load(Ordering::Acquire) {
        pending.push(PendingKernelThread { task, priority });
        return;
    }
    drop(pending);
    register_kernel_thread(task, priority);
}

fn enable_kernel_thread_registration() {
    // Publish readiness and detach the pending list while holding the same
    // lock used by enqueue_kernel_thread.  This closes the SMP window where a
    // task could otherwise be appended after bootstrap had already drained.
    let pending = {
        let mut pending = PENDING_KERNEL_THREADS.lock();
        VSCHED2_SCHEDULER_READY.store(true, Ordering::Release);
        core::mem::take(&mut *pending)
    };
    for PendingKernelThread { task, priority } in pending {
        register_kernel_thread(task, priority);
    }
}

/// Creates a normal kernel thread under the scheduler selected for this boot.
///
/// Before vsched2 bootstrap this deliberately uses `axtask::new_raw`, not
/// `spawn_raw`, so the task cannot become stranded in the legacy AxRunQueue.
/// The entry closure itself is unchanged and may continue using `block_on`.
pub fn spawn_kernel_thread<F>(
    entry: F,
    name: String,
    stack_size: usize,
    priority: isize,
) -> axtask::AxTaskRef
where
    F: FnOnce() + Send + 'static,
{
    if !VSCHED2_PREPARED.load(Ordering::Acquire) {
        return axtask::spawn_raw(entry, name, stack_size);
    }

    let task = axtask::new_raw(entry, name, stack_size);
    enqueue_kernel_thread(task.clone(), priority);
    task
}

pub fn process_init(vspace: *mut ()) -> usize {
    libvsched2::process_init(vspace)
}

pub fn process_drop(pid: usize) {
    libvsched2::process_drop(pid)
}

pub fn user_init_with_vspace(vspace: *mut ()) {
    libvsched2::user_init(vspace);
}

pub fn push_task_into_process(task: *const (), pid: usize) -> bool {
    libvsched2::push_task_into_process(task, pid)
}

pub fn map_vdso_for_child(vspace: *mut ()) -> usize {
    vdso::map_so(vspace as usize) as usize
}

pub fn current_task_ptr() -> *const () {
    libvsched2::current_task_ptr()
}

/// Captures the task that currently owns a `block_on` continuation.
///
/// axtask cannot depend on libvsched2 directly, so this is registered as a
/// callback during bootstrap and stored in each AxWaker together with the
/// current generation.
pub fn current_block_on_task() -> (*const (), usize) {
    let task = current_task_ptr();
    if task.is_null() {
        return (core::ptr::null(), 0);
    }
    let task_impl = unsafe { &*(task as *const task::VschedTaskImpl) };
    (
        task,
        task_impl.wake_generation.load(Ordering::Acquire),
    )
}

/// Starts or cancels the task-state part of a block_on operation.
///
/// The AxWaker first enters Parking, then this function atomically commits
/// Running -> Blocking, and only afterwards publishes Parked.  Consequently a
/// Waker on another CPU either leaves a notification before this commit or sees
/// a task that is already safe to change from Blocking/Blocked to Ready.
pub fn transition_block_on_task(blocking: bool) -> bool {
    let task = current_task_ptr();
    if task.is_null() {
        return false;
    }
    let task_impl = unsafe { &*(task as *const task::VschedTaskImpl) };
    use libvsched2::{Task as _, TaskState};
    let previous = if blocking {
        task_impl.match_set_state(
            TaskState::Ready,
            TaskState::Blocking,
            TaskState::Blocked,
            TaskState::Exited,
            TaskState::Blocking,
        )
    } else {
        task_impl.match_set_state(
            TaskState::Ready,
            TaskState::Running,
            TaskState::Blocked,
            TaskState::Exited,
            TaskState::Running,
        )
    };
    match (blocking, previous) {
        (true, TaskState::Running) | (false, TaskState::Blocking) => true,
        (_, TaskState::Exited) => false,
        (_, state) => panic!(
            "block_on: invalid task state transition, blocking={blocking}, previous={state:?}"
        ),
    }
}

pub fn wake_blocked_task(task: *const (), generation: usize) -> bool {
    if task.is_null() {
        return false;
    }
    let task_impl = unsafe { &*(task as *const task::VschedTaskImpl) };
    use libvsched2::{Task as _, TaskState};
    if task_impl.wake_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    let previous = task_impl.match_set_state(
        TaskState::Ready,
        TaskState::Running,
        TaskState::Ready,
        TaskState::Exited,
        TaskState::Ready,
    );
    match previous {
        TaskState::Blocked => {
            // This transition is the unique owner of queue insertion.  A
            // second Waker observes Ready and does nothing.  Rolling Ready
            // back to Blocked on failure would lose a concurrent notification,
            // so queue exhaustion is an explicit fatal invariant violation.
            assert!(
                libvsched2::push_task(task),
                "wake_blocked_task: ready queue is full"
            );
            true
        }
        // A wake racing with context save changes Blocking to Ready.  The
        // vsched2 thread-entry path observes Ready and performs the enqueue
        // after the context is safe to resume.
        TaskState::Blocking => true,
        TaskState::Ready => true,
        TaskState::Running | TaskState::Exited => false,
    }
}

pub fn set_current_task_ptr(task: *const ()) {
    libvsched2::set_current_task_ptr(task);
}

/// 设置被服务的用户任务指针（per-CPU）。
pub fn set_trapped_vsched_task(task: *const ()) {
    TRAPPED_VSCHED_TASK[<smp::VschedSmpImpl as libvsched2::SMP>::cpu_id()]
        .store(task as usize, Ordering::Release);
}

/// 读取当前被 trap_handler 服务的用户任务（不被 CURRENT_TASK 影响）。
pub fn trapped_vsched_task() -> *const () {
    let current = current_task_ptr() as *const task::VschedTaskImpl;
    if !current.is_null() {
        let owner = unsafe { &*current }.trap_owner.load(Ordering::Acquire);
        if owner != 0 {
            return owner as *const ();
        }
    }
    TRAPPED_VSCHED_TASK[<smp::VschedSmpImpl as libvsched2::SMP>::cpu_id()]
        .load(Ordering::Acquire) as *const ()
}

/// Clears the currently serviced user task after the syscall dispatcher
/// returns.  Compare-exchange avoids erasing a newer nested association.
pub fn clear_trapped_vsched_task(task: *const ()) {
    let slot = &TRAPPED_VSCHED_TASK[<smp::VschedSmpImpl as libvsched2::SMP>::cpu_id()];
    let _ = slot.compare_exchange(
        task as usize,
        0,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}

/// 分配一个用于 vsched2 的 Stack 对象
pub fn alloc_stack() -> *mut () {
    stack::VschedStackImpl::alloc()
}

/// 标记 vsched2 任务为 Exited，让 trap_handler 不再将其推回调度器
pub fn set_vsched_task_exited(task: *const ()) {
    let vti = unsafe { &*(task as *const task::VschedTaskImpl) };
    use libvsched2::Task;
    vti.wake_generation.fetch_add(1, Ordering::AcqRel);
    vti.set_state(libvsched2::TaskState::Exited);

    // The legacy startup path joins the initial userspace task and then powers
    // off in `main`.  The vsched2 bootstrap is a non-returning scheduler root,
    // so perform the equivalent lifecycle transition once the same task has
    // committed Exited.  Child-process exits must continue through wait4 and
    // therefore must not reach this branch.
    if VSCHED2_INIT_TASK
        .compare_exchange(
            task as usize,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        axhal::power::system_off();
    }
}

pub fn vsched2_bootstrap(init_task_ptr: Option<*const ()>, vspace: Option<*mut ()>) -> ! {
    axhal::asm::disable_irqs();
    init_vsched2_interfaces();

    // Install the complete block_on backend before publishing vsched2 as
    // active.  register_vsched2_yield() is the final Release publication;
    // callers that observe it through vsched2_active()'s Acquire load must
    // also observe every callback registered above it.
    axtask::register_block_on_toggle(trap::toggle_handler);
    axtask::register_block_on_hooks(
        current_block_on_task,
        wake_blocked_task,
        transition_block_on_task,
    );

    // Redirect axtask::yield_now() to vsched2's yield trampoline, replacing
    // the legacy AxRunQueue yield with vsched2 resched.  Keep this last: the
    // same pointer is the scheduler-active flag used by block_on and timer code.
    unsafe extern "C" {
        fn vsched_yield_trampoline() -> !;
    }
    axtask::register_vsched2_yield(vsched_yield_trampoline);

    // Initialize empty AxRunQueue so legacy code paths (AxWaker, timer
    // tick, etc.) that deref it under vsched2 don't LazyInit-panic.
    axtask::init_run_queue_empty();

    let curr = axtask::current();
    let main_ptr = register_task(curr.clone(), LOWEST_PRIORITY, 0, true, None, 0);
    // register_task 为线程统一分配 Stack 对象，内核初始化还需要把它
    // 作为当前栈交给 vsched2。
    let init_stack_ptr = unsafe { &*main_ptr }
        .thread_stack_ptr
        .load(Ordering::Acquire) as *mut ();

    unsafe {
        libvsched2::VDSO_VTABLE
            .kernel_init_main
            .expect("kernel_init_main not in vtable")(init_stack_ptr, main_ptr as *const ());
    }
    enable_kernel_thread_registration();

    if let Some(init_task_ptr) = init_task_ptr {
        assert!(!init_task_ptr.is_null(), "vsched2 init task is null");
        assert_eq!(
            VSCHED2_INIT_TASK.swap(init_task_ptr as usize, Ordering::AcqRel),
            0,
            "vsched2 init task was registered twice",
        );
    }

    if let (Some(init_task_ptr), Some(aspace_ptr)) = (init_task_ptr, vspace) {
        let kernel_root = unsafe { asm::read_user_page_table() };
        if !aspace_ptr.is_null() {
            let aspace = unsafe { &*(aspace_ptr as *const axmm::AddrSpace) };
            let root = aspace.page_table_root();
            // Copy kernel mappings into user AS so kernel code can execute
            // under the user page table without SATP switch on trap entry.
            {
                let user_aspace = unsafe { &mut *(aspace_ptr as *mut axmm::AddrSpace) };
                let kernel_aspace = axmm::kernel_aspace().lock();
                let _ = user_aspace.copy_mappings_from(&kernel_aspace);
            }
            if root.as_usize() != 0 && root != kernel_root {
                unsafe {
                    asm::write_user_page_table(root);
                    asm::flush_tlb(None);
                    core::arch::asm!("csrs sstatus, {}", in(reg) 1usize << 18);
                }
            }
// axlog::ax_println!("vsched2: calling process_init...");
        }
// axlog::ax_println!("vsched2: process_init pid={}", pid);
        let pid = libvsched2::process_init(aspace_ptr);
// axlog::ax_println!("[verify] vdso_pa={:#x} user_vdso_base={:#x}",
        // --- Verification ---
        // user_init must run with the user PT active so that &USER_SCHEDULER
        // inside init_sources resolves to the user vDSO copy.  We call
        // user_init_with_vspace which translates the address to kva.
        libvsched2::user_init(aspace_ptr);
        libvsched2::push_task_into_process(init_task_ptr, pid);
        unsafe {
            asm::write_user_page_table(kernel_root);
            asm::flush_tlb(None);
        }
    }

    // Sync any new kernel mappings from process_init into user PT.
    if let Some(aspace_ptr) = vspace {
        if !aspace_ptr.is_null() {
            let mut user_aspace = unsafe { &mut *(aspace_ptr as *mut axmm::AddrSpace) };
            let kernel_aspace = axmm::kernel_aspace().lock();
            let _ = user_aspace.copy_mappings_from(&kernel_aspace);
        }
    }

    activate_vsched_trap_vector();
    curr.set_state(AxTaskState::Blocked);

    // `raw_kschedule` is the documented initialization entry.  The bootstrap
    // execution flow remains the per-CPU scheduler wait context and is never
    // inserted into the normal ready queue.
    let entry = unsafe {
        libvsched2::VDSO_VTABLE
            .raw_kschedule
            .expect("raw_kschedule not in vtable")
    };
    unsafe {
        core::arch::asm!(
            "li s1, 0",
            "li s2, 0",
            "jalr {entry}",
            entry = in(reg) entry,
            options(noreturn),
        );
    }
}
