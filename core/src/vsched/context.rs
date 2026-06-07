use axmm::AddrSpace;
use memory_addr::PhysAddr;

use super::trapframe::UserTrapFrame;
use super::task::VschedTaskImpl;

pub struct VschedContextImpl;

pub struct VschedVSpaceImpl;

pub fn activate_user_aspace(root: PhysAddr) {
    let current_root = axhal::asm::read_user_page_table();
    if current_root != root {
        unsafe {
            axhal::asm::write_user_page_table(root);
            axhal::asm::flush_tlb(None);
            #[cfg(target_arch = "riscv64")]
            core::arch::asm!("csrs sstatus, {}", in(reg) 1usize << 18);
        };
    }
}

pub fn page_table_root_from_raw(ptr: *const ()) -> Option<PhysAddr> {
    if ptr.is_null() {
        return None;
    }
    let root = unsafe { &*(ptr as *const AddrSpace) }.page_table_root();
    if root.as_usize() == 0 {
        return None;
    }
    Some(root)
}

pub const VSCHED2_INTO_KERNEL_SYSNO: usize = 0xdead;

impl libvsched2::Context for VschedContextImpl {
    fn into_kernel() -> ! {
        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!(
                "ecall",
                in("a7") VSCHED2_INTO_KERNEL_SYSNO,
                options(noreturn),
            );
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
            unimplemented!("VschedContextImpl::into_kernel: unsupported architecture");
        }
    }

    fn into_user(ustack: usize) {
        let entry = unsafe {
            libvsched2::VDSO_VTABLE
                .raw_run_task
                .expect("VDSO_VTABLE.raw_run_task not initialized") as usize
        };

        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!(
                "csrw   sepc, {entry}",
                "li     t0, (1 << 8)",
                "csrc   sstatus, t0",
                "mv     sp, {sp}",
                "sret",
                entry = in(reg) entry,
                sp = in(reg) ustack,
                options(noreturn),
            );
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
            unimplemented!("VschedContextImpl::into_user: unsupported architecture");
        }
    }

    fn into_user_context(task: *const ()) {
        let tf_ptr = unsafe {
            let vsched_task = &*(task as *const VschedTaskImpl);
            vsched_task.trap_frame.load(core::sync::atomic::Ordering::Acquire)
        };
        assert_ne!(tf_ptr, 0, "into_user_context: trap_frame is null");
        let tf = unsafe { &*(tf_ptr as *const UserTrapFrame) };
        unsafe { tf.restore_and_sret() };
    }
}

impl libvsched2::VSpace for VschedVSpaceImpl {
    fn into_vspace(vspace: *mut ()) {
        if let Some(root) = page_table_root_from_raw(vspace) {
            activate_user_aspace(root);
        }
    }
}
