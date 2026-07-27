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
    libvsched2::push_task_into_current(task_ptr)
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
}

pub fn vsched2_bootstrap(init_task_ptr: Option<*const ()>, vspace: Option<*mut ()>) -> ! {
    axhal::asm::disable_irqs();
    init_vsched2_interfaces();

    // Redirect axtask::yield_now() to vsched2's yield trampoline,
    // replacing the legacy AxRunQueue yield with vsched2 resched.
    unsafe extern "C" {
        fn vsched_yield_trampoline() -> !;
    }
    axtask::register_vsched2_yield(vsched_yield_trampoline);
    axtask::register_block_on_toggle(trap::toggle_handler);
    axtask::register_block_on_hooks(
        current_block_on_task,
        wake_blocked_task,
        transition_block_on_task,
    );

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

    if let (Some(init_task_ptr), Some(aspace_ptr)) = (init_task_ptr, vspace) {
        let kernel_root = unsafe { asm::read_user_page_table() };
        if !aspace_ptr.is_null() {
            let aspace = unsafe { &*(aspace_ptr as *const axmm::AddrSpace) };
            let root = aspace.page_table_root();
            // Copy kernel mappings into user AS so kernel code can execute
            // under the user page table without SATP switch on trap entry.
            {
                let mut user_aspace = unsafe { &mut *(aspace_ptr as *mut axmm::AddrSpace) };
                let kernel_aspace = axmm::kernel_aspace().lock();
                let _ = user_aspace.copy_mappings_from(&kernel_aspace);

                // Fill the vDSO reserved gap so mmap won't allocate here
// axlog::ax_println!("[vsched2] ext vdso_base={:#x} vdso_size={:#x}",
                let vdso_base = user_aspace.vdso_base;
                if vdso_base != 0 {
                    let vdso_size = unsafe { vdso::VDSO_SIZE };
                    let vdso_end = vdso_base + vdso_size;
                    let highest = user_aspace.areas()
                        .filter(|a| a.start().as_usize() >= vdso_base
                                && a.end().as_usize() <= vdso_end)
                        .map(|a| a.end().as_usize())
                        .max()
// axlog::ax_println!("[vsched2] ext highest={:#x} vdso_end={:#x}", highest, vdso_end);
                        .unwrap_or(vdso_base);
                    if highest < vdso_end {
                        let gap = vdso_end - highest;
                        user_aspace.map(
                            memory_addr::VirtAddr::from(highest),
                            gap,
                            axhal::paging::MappingFlags::READ
                                | axhal::paging::MappingFlags::WRITE
                                | axhal::paging::MappingFlags::USER,
                            false,
                            axmm::backend::Backend::new_alloc(
                                memory_addr::VirtAddr::from(highest),
                                axhal::paging::PageSize::Size4K,
                            ),
// axlog::ax_println!("[vsched2] extended vdso reserved: {:#x}-{:#x} gap={:#x}", highest, vdso_end, gap);
                        ).expect("vsched2: extend vdso reserved failed");
                    }
                }
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

    loop {
        unsafe {
            core::arch::asm!("call vsched_yield_trampoline");
        }
    }
}
