use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use alloc::alloc::{Layout, alloc};
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::string;

use axtask::TaskState as AxTaskState;
use axmm;
use axhal::{mem::phys_to_virt, asm};

use libvsched2::{self, Stack as _};
use vdso;

pub mod context;
mod smp;
pub mod stack;
pub mod task;
pub mod trap;
pub mod trapframe;
mod trap_vector;
mod userdata;

pub use task::{CoroutinePoll, register_task, task_from_raw, VschedTaskImpl};

use crate::config;

pub static mut VSCHED2_VVAR_START_PA: usize = 0;
pub static mut VSCHED2_VVAR_SIZE: usize = 0;
pub static mut VSCHED2_VDSO_START_PA: usize = 0;
pub static mut VSCHED2_VDSO_SIZE: usize = 0;

static VSCHED2_READY: AtomicBool = AtomicBool::new(false);

/// 内核 SATP 值，在 activate_vsched_trap_vector 时写入, trap 向量直接加载
pub static KERNEL_SATP_VAL: AtomicUsize = AtomicUsize::new(0);
/// trap 向量暂存用户 t0
pub static TRAP_SCRATCH: AtomicUsize = AtomicUsize::new(0);
/// 内核 gp 值, activate_vsched_trap_vector 时记录, trap 向量恢复 (per-CPU access)
pub static KERNEL_GP: AtomicUsize = AtomicUsize::new(0);
/// 最近一次进入用户态前保存的任务指针
pub static LAST_USER_TASK: AtomicUsize = AtomicUsize::new(0);
/// 当前活跃用户页表根, 用于陷阱向量进入 VDSO 前切换页表
pub static LAST_USER_PT_ROOT: AtomicUsize = AtomicUsize::new(0);
pub static KERNEL_VVAR_BASE: AtomicUsize = AtomicUsize::new(0);
pub static KERNEL_KSCHEDULER: AtomicUsize = AtomicUsize::new(0);

pub const HIGHEST_PRIORITY: isize = 0;
pub const LOWEST_PRIORITY: isize = 15;

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

    // Dump VDSO_VTABLE to verify no BSS entries
}

unsafe extern "C" {
    fn vsched2_trap_vector();
}

pub fn activate_vsched_trap_vector() {
    // Layout: [pre-save stack 256KB][kernel SATP 8B]
    let layout = alloc::alloc::Layout::from_size_align(262144 + 8, 16).unwrap();
    let raw: *mut u8 = unsafe { alloc::alloc::alloc(layout) };
    assert!(!raw.is_null(), "failed to alloc pre-save stack");
    let stack_top = raw as usize + 262144;
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
        axlog::ax_println!("vsched2: stvec={:#x} sie={:#x} sstatus={:#x}",
            stvec_val, sie_val, sstatus_val);
        core::arch::asm!("csrw sscratch, {}", in(reg) stack_top);
        axhal::asm::write_trap_vector_base(vsched2_trap_vector as *const () as usize);
    }
}

pub fn push_task_to_kernel(task_ptr: *const ()) {
    libvsched2::push_task_into_current(task_ptr);
}

/// 分配一个用于 vsched2 的 Stack 对象
pub fn alloc_stack() -> *mut () {
    stack::VschedStackImpl::alloc()
}

pub fn vsched2_bootstrap(init_task_ptr: Option<*const ()>, vspace_ptr: Option<*mut *mut ()>) -> ! {
    axhal::asm::disable_irqs();
    init_vsched2_interfaces();

    let curr = axtask::current();
    let main_ptr = register_task(curr.clone(), HIGHEST_PRIORITY, 0, None);
    // 分配一个 Stack 对象作为内核主任务的初始栈
    let init_stack_ptr = alloc_stack();
    unsafe { (main_ptr as *mut VschedTaskImpl).as_mut().unwrap() }
        .thread_stack_ptr.store(init_stack_ptr as usize, Ordering::Release);

    unsafe {
        libvsched2::VDSO_VTABLE.kernel_init_main
            .expect("kernel_init_main not in vtable")(
                init_stack_ptr,
                main_ptr as *const (),
            );
    }
    axlog::ax_println!("vsched2: kernel_init_main done");

    if let (Some(init_task_ptr), Some(vspace_ptr)) = (init_task_ptr, vspace_ptr) {
        let kernel_root = unsafe { asm::read_user_page_table() };
        let aspace_ptr = unsafe { *vspace_ptr };
        if !aspace_ptr.is_null() {
            let aspace = unsafe { &*(aspace_ptr as *const axmm::AddrSpace) };
            let root = aspace.page_table_root();
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
        libvsched2::push_task_into_current(init_task_ptr);
        unsafe {
            asm::write_user_page_table(kernel_root);
            asm::flush_tlb(None);
        }
    }

    activate_vsched_trap_vector();
    axlog::ax_println!("vsched2: trap vector active, entering scheduler");

    // 将所有最新内核映射同步到用户页表。
    if let Some(vspace_ptr) = vspace_ptr {
        let aspace_ptr = unsafe { *vspace_ptr };
        if !aspace_ptr.is_null() {
            let mut user_aspace = unsafe { &mut *(aspace_ptr as *mut axmm::AddrSpace) };
            let kernel_aspace = axmm::kernel_aspace().lock();
            let _ = user_aspace.copy_mappings_from(&kernel_aspace);
        }
    }

    axlog::ax_println!("vsched2: entering yield loop");
    loop {
        unsafe {
            core::arch::asm!("call vsched_yield_trampoline");
        }
    }
}
