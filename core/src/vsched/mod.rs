use alloc::{
    alloc::{Layout, alloc},
    boxed::Box,
    string,
    sync::Arc,
};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axhal::asm;
use axmm;
use axtask::TaskState as AxTaskState;
use libvsched2::{self, Stack as _};
use vdso;

pub mod context;
mod smp;
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
/// trap 向量在 sscratch 被覆盖前保存的原始值, trap_entry 用于回收旧预保存栈
pub static SAVED_SSCRATCH: AtomicUsize = AtomicUsize::new(0);
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
pub static PRE_SAVE_BASE: AtomicUsize = AtomicUsize::new(0);
pub static PRE_SAVE_TOP: AtomicUsize = AtomicUsize::new(0);

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
    }
}

pub fn from_vsched_state(s: libvsched2::TaskState) -> AxTaskState {
    match s {
        libvsched2::TaskState::Ready => AxTaskState::Ready,
        libvsched2::TaskState::Running => AxTaskState::Running,
        libvsched2::TaskState::Blocked => AxTaskState::Blocked,
        libvsched2::TaskState::Blocking => AxTaskState::Blocked,
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
    // Layout: [pre-save stack 256KB][kernel SATP 8B]
    let layout = alloc::alloc::Layout::from_size_align(262144 + 8, 16).unwrap();
    let raw: *mut u8 = unsafe { alloc::alloc::alloc(layout) };
    assert!(!raw.is_null(), "failed to alloc pre-save stack");
    let stack_base = raw as usize;
    let stack_top = raw as usize + 262144;
    PRE_SAVE_BASE.store(raw as usize, Ordering::Release);
    PRE_SAVE_TOP.store(stack_top, Ordering::Release);
    let kernel_satp: usize;
    unsafe {
        core::arch::asm!("csrr {}, satp", out(reg) kernel_satp);
        let slot = stack_top as *mut usize;
        slot.write_volatile(kernel_satp);
    }
    KERNEL_SATP_VAL.store(kernel_satp, Ordering::Release);
    let kernel_gp: usize;
    unsafe { core::arch::asm!("mv {}, gp", out(reg) kernel_gp) };
    KERNEL_GP.store(kernel_gp, Ordering::Release);
    unsafe {
        let stvec_val: usize;
        let sie_val: usize;
        let sstatus_val: usize;
        core::arch::asm!(
            "csrr {}, stvec",
            "csrr {}, sie",
            "csrr {}, sstatus",
            out(reg) stvec_val, out(reg) sie_val, out(reg) sstatus_val,
        );
        axlog::ax_println!(
            "vsched2: stvec={:#x} sie={:#x} sstatus={:#x}",
            stvec_val,
            sie_val,
            sstatus_val
        );
        core::arch::asm!("csrw sscratch, {}", in(reg) stack_top);
        axhal::asm::write_trap_vector_base(vsched2_trap_vector as *const () as usize);
    }
}

pub fn push_task_to_kernel(task_ptr: *const ()) {
    libvsched2::push_task_into_current(task_ptr);
}

pub fn process_init(vspace_ptr: *mut *mut ()) -> usize {
    libvsched2::process_init(vspace_ptr)
}

pub fn process_reinit(vspace_ptr: *mut *mut (), pid: usize) {
    libvsched2::process_reinit(vspace_ptr, pid);
}

pub fn user_init_with_vspace(vspace: *mut ()) {
    libvsched2::user_init_with_vspace(vspace);
}

pub fn user_init() {
    libvsched2::user_init()
}

pub fn push_task_into_process(task: *const (), pid: usize) -> bool {
    libvsched2::push_task_into_process(task, pid)
}

/// 为子进程重新映射 vDSO/VVAR。等效于 load_user_app 中的 map_so 调用，
/// 给子进程分配独立的 .data/.bss 物理页（.bss 为全零）。
pub fn map_vdso_for_child(vspace: *mut ()) -> usize {
    vdso::map_so(vspace as usize) as usize
}

/// 读取 vsched2 的当前任务指针 (CURRENT_TASK in VVAR)。
pub fn current_task_ptr() -> *const () {
    libvsched2::current_task_ptr()
}

/// 设置 vsched2 的当前任务指针 (CURRENT_TASK in VVAR)。
pub fn set_current_task_ptr(task: *const ()) {
    axlog::ax_println!("[vsched::set_current] task={:#x}", task as usize);
    libvsched2::set_current_task_ptr(task);
}

/// 分配一个用于 vsched2 的 Stack 对象
pub fn alloc_stack() -> *mut () {
    stack::VschedStackImpl::alloc()
}

/// 标记 vsched2 任务为 Exited，让 trap_handler 不再将其推回调度器
pub fn set_vsched_task_exited(task: *const ()) {
    let vti = unsafe { &*(task as *const task::VschedTaskImpl) };
    use libvsched2::Task;
    vti.set_state(libvsched2::TaskState::Exited);
}

pub fn vsched2_bootstrap(init_task_ptr: Option<*const ()>, vspace_ptr: Option<*mut *mut ()>) -> ! {
    axhal::asm::disable_irqs();
    init_vsched2_interfaces();

    // Redirect axtask::yield_now() to vsched2's yield trampoline,
    // replacing the legacy AxRunQueue yield with vsched2 resched.
    unsafe extern "C" {
        fn vsched_yield_trampoline() -> !;
    }
    axtask::register_vsched2_yield(vsched_yield_trampoline);

    // Initialize empty AxRunQueue so legacy code paths (AxWaker, timer
    // tick, etc.) that deref it under vsched2 don't LazyInit-panic.
    axtask::init_run_queue_empty();

    let curr = axtask::current();
    let main_ptr = register_task(curr.clone(), LOWEST_PRIORITY, 0, None);
    // 分配一个 Stack 对象作为内核主任务的初始栈
    let init_stack_ptr = alloc_stack();
    unsafe { (main_ptr as *mut VschedTaskImpl).as_mut().unwrap() }
        .thread_stack_ptr
        .store(init_stack_ptr as usize, Ordering::Release);

    unsafe {
        libvsched2::VDSO_VTABLE
            .kernel_init_main
            .expect("kernel_init_main not in vtable")(init_stack_ptr, main_ptr as *const ());
    }
    axlog::ax_println!("vsched2: kernel_init_main done");

    if let (Some(init_task_ptr), Some(vspace_ptr)) = (init_task_ptr, vspace_ptr) {
        let kernel_root = unsafe { asm::read_user_page_table() };
        let aspace_ptr = unsafe { *vspace_ptr };
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
                let vdso_base = user_aspace.vdso_base;
                axlog::ax_println!("[vsched2] ext vdso_base={:#x} vdso_size={:#x}",
                    vdso_base, unsafe { vdso::VDSO_SIZE });
                if vdso_base != 0 {
                    let vdso_size = unsafe { vdso::VDSO_SIZE };
                    let vdso_end = vdso_base + vdso_size;
                    let highest = user_aspace.areas()
                        .filter(|a| a.start().as_usize() >= vdso_base
                                && a.end().as_usize() <= vdso_end)
                        .map(|a| a.end().as_usize())
                        .max()
                        .unwrap_or(vdso_base);
                    axlog::ax_println!("[vsched2] ext highest={:#x} vdso_end={:#x}", highest, vdso_end);
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
                        ).expect("vsched2: extend vdso reserved failed");
                        axlog::ax_println!("[vsched2] extended vdso reserved: {:#x}-{:#x} gap={:#x}", highest, vdso_end, gap);
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
        }
        axlog::ax_println!("vsched2: calling process_init...");
        let pid = libvsched2::process_init(vspace_ptr);
        axlog::ax_println!("vsched2: process_init pid={}", pid);
        // --- Verification ---
        axlog::ax_println!("[verify] vdso_pa={:#x} user_vdso_base={:#x}",
            unsafe { VSCHED2_VDSO_START_PA },
            crate::vsched::context::get_process_vdso_base());
        axlog::ax_println!("vsched2: calling user_init_with_vspace...");
        // user_init must run with the user PT active so that &USER_SCHEDULER
        // inside init_sources resolves to the user vDSO copy.  We call
        // user_init_with_vspace which translates the address to kva.
        let aspace_ptr = unsafe { *vspace_ptr };
        libvsched2::user_init_with_vspace(aspace_ptr);
        axlog::ax_println!("vsched2: user_init done");
        let pushed = libvsched2::push_task_into_process(init_task_ptr, pid);
        axlog::ax_println!(
            "vsched2: push_task_into_process pid={} result={}",
            pid,
            pushed
        );
        unsafe {
            asm::write_user_page_table(kernel_root);
            asm::flush_tlb(None);
        }
    }

    // Sync any new kernel mappings from process_init into user PT.
    if let Some(vspace_ptr) = vspace_ptr {
        let aspace_ptr = unsafe { *vspace_ptr };
        if !aspace_ptr.is_null() {
            let mut user_aspace = unsafe { &mut *(aspace_ptr as *mut axmm::AddrSpace) };
            let kernel_aspace = axmm::kernel_aspace().lock();
            let _ = user_aspace.copy_mappings_from(&kernel_aspace);
        }
    }

    activate_vsched_trap_vector();
    axlog::ax_println!("vsched2: trap vector active, entering scheduler");

    axlog::ax_println!("vsched2: entering yield loop");
    loop {
        unsafe {
            core::arch::asm!("call vsched_yield_trampoline");
        }
    }
}
