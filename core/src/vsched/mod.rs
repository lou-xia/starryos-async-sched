use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use alloc::alloc::{Layout, alloc};

use axtask::TaskState as AxTaskState;

use libvsched2;
use vdso;

pub mod context;
mod smp;
mod stack;
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
}

unsafe extern "C" {
    fn vsched2_trap_vector();
}

pub fn activate_vsched_trap_vector() {
    let init_stack = unsafe {
        alloc(Layout::from_size_align(config::KERNEL_STACK_SIZE, 16).unwrap())
    };
    assert!(!init_stack.is_null(), "failed to allocate initial sscratch stack");
    unsafe {
        core::arch::asm!("csrw sscratch, {}", in(reg) init_stack);
        axhal::asm::write_trap_vector_base(vsched2_trap_vector as *const () as usize);
    }
}
