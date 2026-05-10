use axhal::mem::phys_to_virt;
use axmm::AddrSpace;
use axtask;
use memory_addr::{PhysAddr, VirtAddr};

use crate::task::AsThread;

use super::{VSCHED2_VVAR_SIZE, VSCHED2_VVAR_START_PA};

pub struct VschedUserDataImpl;

fn find_user_vaddr_for_phys(aspace: &AddrSpace, target: PhysAddr) -> Option<VirtAddr> {
    for area in aspace.areas() {
        let mut vaddr = area.start();
        while vaddr < area.end() {
            if let Ok((paddr, ..)) = aspace.page_table().query(vaddr)
                && paddr == target
            {
                return Some(vaddr);
            }
            vaddr += 4096;
        }
    }
    None
}

impl libvsched2::UserData for VschedUserDataImpl {
    fn get_user_data(pos: usize, len: usize, vspace: Option<*mut ()>) -> *mut () {
        let vvar_start_pa = unsafe { VSCHED2_VVAR_START_PA };
        let vvar_size = unsafe { VSCHED2_VVAR_SIZE };

        if len > vvar_size {
            return core::ptr::null_mut();
        }

        let kernel_vvar_start = phys_to_virt(PhysAddr::from(vvar_start_pa)).as_usize();
        let kernel_vvar_end = kernel_vvar_start + vvar_size;
        let end = match pos.checked_add(len) {
            Some(e) => e,
            None => return core::ptr::null_mut(),
        };

        if pos < kernel_vvar_start || end > kernel_vvar_end {
            return core::ptr::null_mut();
        }

        let offset = pos - kernel_vvar_start;
        let target_page_pa = PhysAddr::from((vvar_start_pa + offset) & !0xfff);

        let user_page = if let Some(vspace_ptr) = vspace {
            if vspace_ptr.is_null() {
                return core::ptr::null_mut();
            }
            let aspace_ref = unsafe { &*(vspace_ptr as *const AddrSpace) };
            let Some(page) = find_user_vaddr_for_phys(aspace_ref, target_page_pa) else {
                return core::ptr::null_mut();
            };
            page
        } else {
            let current = axtask::current();
            let Some(thr) = current.try_as_thread() else {
                return core::ptr::null_mut();
            };
            let aspace = thr.proc_data.aspace.lock();
            let Some(page) = find_user_vaddr_for_phys(&aspace, target_page_pa) else {
                return core::ptr::null_mut();
            };
            page
        };

        (user_page.as_usize() + offset % 4096) as *mut ()
    }
}
