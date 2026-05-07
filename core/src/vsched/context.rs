use axmm::AddrSpace;
use memory_addr::PhysAddr;

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

impl libvsched2::Context for VschedContextImpl {
    // TODO
    fn into_kernel() -> ! {
        panic!("VschedContextImpl::into_kernel: vsched2 trap entry not yet integrated");
    }

    // TODO
    fn into_user(_ustack: usize) {
        panic!("VschedContextImpl::into_user: vsched2 user trampoline not yet integrated");
    }

    // TODO
    fn into_user_context(_task: *const ()) {
        panic!("VschedContextImpl::into_user_context: vsched2 user trampoline not yet integrated");
    }

    fn switch_vspace(vspace_pid: *const ()) {
        if let Some(root) = page_table_root_from_raw(vspace_pid) {
            activate_user_aspace(root);
        }
    }
}

impl libvsched2::VSpace for VschedVSpaceImpl {
    fn into_vspace(vspace: *mut ()) {
        if let Some(root) = page_table_root_from_raw(vspace) {
            activate_user_aspace(root);
        }
    }
}
