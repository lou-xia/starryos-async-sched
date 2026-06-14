//! VschedUserDataImpl — vsched2 UserData 接口实现。
//!
//! 将内核侧 vDSO 中的共享数据（vVAR）和私有数据（.data/.bss）地址
//! 翻译为用户地址空间中对应的虚拟地址，供 vsched2 调度器在用户态访问。

use axhal::mem::phys_to_virt;
use axmm::AddrSpace;
use axtask;
use memory_addr::{PhysAddr, VirtAddr};
use crate::task::AsThread;
use super::{VSCHED2_VVAR_SIZE, VSCHED2_VVAR_START_PA, VSCHED2_VDSO_START_PA, VSCHED2_VDSO_SIZE};
use super::context::get_process_vdso_base;

pub struct VschedUserDataImpl;

/// 在地址空间中查找目标物理页对应的用户虚拟地址。
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
        let end = match pos.checked_add(len) {
            Some(e) => e,
            None => return core::ptr::null_mut(),
        };

        // 路径 1：vVAR 共享数据区 — 内核和用户空间映射到同一物理页。
        {
            let vvar_start_pa = unsafe { VSCHED2_VVAR_START_PA };
            let vvar_size = unsafe { VSCHED2_VVAR_SIZE };
            let kernel_vvar_start = phys_to_virt(PhysAddr::from(vvar_start_pa)).as_usize();
            let kernel_vvar_end = kernel_vvar_start + vvar_size;

            if pos >= kernel_vvar_start && end <= kernel_vvar_end {
                let offset = pos - kernel_vvar_start;
                let target_page_pa = PhysAddr::from((vvar_start_pa + offset) & !0xfff);

                let user_page = if let Some(vspace_ptr) = vspace {
                    if vspace_ptr.is_null() { return core::ptr::null_mut(); }
                    let aspace_ref = unsafe { &*(vspace_ptr as *const AddrSpace) };
                    let Some(page) = find_user_vaddr_for_phys(aspace_ref, target_page_pa)
                        else { return core::ptr::null_mut() };
                    page
                } else {
                    let current = axtask::current();
                    let Some(thr) = current.try_as_thread()
                        else { return core::ptr::null_mut() };
                    let aspace = thr.proc_data.aspace.lock();
                    let Some(page) = find_user_vaddr_for_phys(&aspace, target_page_pa)
                        else { return core::ptr::null_mut() };
                    page
                };
                return (user_page.as_usize() + offset % 4096) as *mut ();
            }
        }

        // 路径 2：vDSO 私有数据段（.data/.bss）—— 内核和用户映射到
        // 不同的物理页，但同一符号在 .so 内的偏移量相同。
        {
            let vdso_start_pa = unsafe { VSCHED2_VDSO_START_PA };
            let vdso_size = unsafe { VSCHED2_VDSO_SIZE };
            let kernel_vdso_start = phys_to_virt(PhysAddr::from(vdso_start_pa)).as_usize();
            let kernel_vdso_end = kernel_vdso_start + vdso_size;

            if pos >= kernel_vdso_start && end <= kernel_vdso_end {
                let offset = pos - kernel_vdso_start;
                let user_vdso_base = get_process_vdso_base();
                if user_vdso_base == 0 {
                    return core::ptr::null_mut();
                }
                return (user_vdso_base + offset) as *mut ();
            }
        }

        core::ptr::null_mut()
    }
}
