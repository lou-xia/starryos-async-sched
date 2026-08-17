//! VschedUserDataImpl — vsched2 UserData 接口实现。
//!
//! 5 步地址转换（根据 vsched2 接口注释实现）:
//!   1. 内核符号 → .so 内偏移:  offset = pos - kernel_vdso_start
//!   2. 用户 vDSO 基址:            user_vdso_base = get_process_vdso_base()
//!   3. 用户虚拟地址:              user_va = user_vdso_base + offset
//!   4. 查页表 → PA:               page_table.query(user_va) → pa
//!   5. PA → 内核 VA:              phys_to_virt(pa) + page_offset → kva
//!
//! Case 1 (vVAR): 内核/用户共享物理页，直接返回 pos（内核 VA）。
//!
//! vspace 参数语义（来自接口注释）:
//! - Some(kernel_ptr): 直接指向 AddrSpace 结构体（process_init 调用路径）
//! - None:             当前地址空间
//!
//! 兼容说明：vsched2 当前的 `current::get_user_data()` 会把 None 改写为
//! `Some(CURRENT_VSPACE as *mut ())`，因此实现还必须接受小整数 pid。

use axhal::mem::{phys_to_virt, virt_to_phys};
use axmm::AddrSpace;
use libvsched2::SMP;
use memory_addr::{PhysAddr, VirtAddr};

use super::{
    VSCHED2_VDSO_SIZE, VSCHED2_VDSO_START_PA, VSCHED2_VVAR_SIZE, VSCHED2_VVAR_START_PA,
    context::CURRENT_VDSO_BASE, smp::VschedSmpImpl,
};

pub struct VschedUserDataImpl;

const PAGE_SIZE: usize = 0x1000;
const KERNEL_BASE: usize = 0xffffffc000000000;

/// Translates a user-vDSO range through an explicitly supplied address space.
/// The vDSO loader uses linear mappings for each segment; verify physical
/// contiguity instead of assuming that translating only the first byte is
/// sufficient for the interface's complete-range guarantee.
fn translate_user_range(aspace: &AddrSpace, user_va: usize, len: usize) -> Option<*mut ()> {
    let end = user_va.checked_add(len)?;
    let first_page = user_va & !(PAGE_SIZE - 1);
    let last_page = if len == 0 {
        first_page
    } else {
        (end - 1) & !(PAGE_SIZE - 1)
    };
    let (first_pa, ..) = aspace.page_table().query(VirtAddr::from(first_page)).ok()?;

    let mut page = first_page;
    while page <= last_page {
        let (pa, ..) = aspace.page_table().query(VirtAddr::from(page)).ok()?;
        if pa.as_usize() != first_pa.as_usize() + (page - first_page) {
            return None;
        }
        page = page.checked_add(PAGE_SIZE)?;
    }

    let page_offset = user_va - first_page;
    Some((phys_to_virt(first_pa).as_usize() + page_offset) as *mut ())
}

/// 在目标地址空间中寻找内核线性地址对应的用户地址。
///
/// 该反向查询只用于初始化或注册事件源。若同一物理范围在目标地址空间中有
/// 多个用户映射，则无法确定调用方期望的地址，返回`None`。
fn find_user_range(aspace: &AddrSpace, kernel_va: usize, len: usize) -> Option<usize> {
    kernel_va.checked_add(len)?;
    let kernel_page = kernel_va & !(PAGE_SIZE - 1);
    let page_offset = kernel_va - kernel_page;
    let first_pa = virt_to_phys(VirtAddr::from(kernel_page));
    let mut found = None;

    for area in aspace.areas() {
        let mut user_page = area.start().as_usize();
        let area_end = area.end().as_usize();
        while user_page < area_end {
            let Ok((pa, ..)) = aspace.page_table().query(VirtAddr::from(user_page)) else {
                user_page += PAGE_SIZE;
                continue;
            };
            if pa == first_pa {
                let user_va = user_page.checked_add(page_offset)?;
                if translate_user_range(aspace, user_va, len)
                    .is_some_and(|translated| translated as usize == kernel_va)
                {
                    match found {
                        Some(old) if old != user_va => return None,
                        Some(_) => {}
                        None => found = Some(user_va),
                    }
                }
            }
            user_page += PAGE_SIZE;
        }
    }
    found
}

impl libvsched2::UserData for VschedUserDataImpl {
    fn get_user_data(pos: usize, len: usize, vspace: Option<*mut ()>) -> *mut () {
        let end = match pos.checked_add(len) {
            Some(e) => e,
            None => return core::ptr::null_mut(),
        };

        // Case 1: vVAR — 内核和用户映射到同一物理页，直接返回 pos。
        {
            let vvar_start_pa = unsafe { VSCHED2_VVAR_START_PA };
            let vvar_size = unsafe { VSCHED2_VVAR_SIZE };
            let kernel_vvar_start = phys_to_virt(PhysAddr::from(vvar_start_pa)).as_usize();
            let kernel_vvar_end = kernel_vvar_start + vvar_size;

            if pos >= kernel_vvar_start && end <= kernel_vvar_end {
                return pos as *mut ();
            }
        }

        // Case 2: vDSO .data/.bss — per-process COW，5 步转换。
        {
            let vdso_start_pa = unsafe { VSCHED2_VDSO_START_PA };
            let vdso_size = unsafe { VSCHED2_VDSO_SIZE };
            let kernel_vdso_start = phys_to_virt(PhysAddr::from(vdso_start_pa)).as_usize();
            let kernel_vdso_end = kernel_vdso_start + vdso_size;

            if pos >= kernel_vdso_start && end <= kernel_vdso_end {
                // Step 1: offset within .so
                let offset = pos - kernel_vdso_start;

                // Step 2: the documented form is an AddrSpace pointer.  The
                // current vsched2 helper also passes CURRENT_VSPACE as a small
                // integer for the active address space; keep that compatibility
                // until the upstream contract is made consistent.
                let user_vdso_base = match vspace {
                    Some(ptr) if ptr as usize >= KERNEL_BASE => {
                        let aspace = unsafe { &*(ptr as *const AddrSpace) };
                        aspace.vdso_base
                    }
                    Some(_) | None => CURRENT_VDSO_BASE[<VschedSmpImpl as SMP>::cpu_id()]
                        .load(core::sync::atomic::Ordering::Acquire),
                };
                if user_vdso_base == 0 {
                    return core::ptr::null_mut();
                }

                // Step 3: user VA
                let user_va = user_vdso_base + offset;

                // Step 4-5: query page table → PA → kernel VA
                // (diagnostic logging suppressed for performance)
                if let Some(vspace_ptr) = vspace.filter(|ptr| *ptr as usize >= KERNEL_BASE) {
                    let aspace = unsafe { &*(vspace_ptr as *const AddrSpace) };
                    return translate_user_range(aspace, user_va, len)
                        .unwrap_or(core::ptr::null_mut());
                }

                // None is only used for the current address space.  The range
                // is already bounded by the loaded vDSO image above; the
                // scheduler invariant guarantees its page table is active and
                // SUM is set before the kernel dereferences this UVA.
                return user_va as *mut ();
            }
        }

        core::ptr::null_mut()
    }

    fn get_user_addr(pos: usize, len: usize, vspace: Option<*mut ()>) -> *mut () {
        let end = match pos.checked_add(len) {
            Some(end) => end,
            None => return core::ptr::null_mut(),
        };
        let aspace = vspace
            .filter(|ptr| *ptr as usize >= KERNEL_BASE)
            .map(|ptr| unsafe { &*(ptr as *const AddrSpace) });
        let user_vdso_base = aspace.map_or_else(
            || {
                CURRENT_VDSO_BASE[<VschedSmpImpl as SMP>::cpu_id()]
                    .load(core::sync::atomic::Ordering::Acquire)
            },
            |aspace| aspace.vdso_base,
        );
        if user_vdso_base == 0 {
            return core::ptr::null_mut();
        }

        // 内核初始vDSO映射中的地址A可以直接按ELF内偏移换算为用户地址B。
        let vdso_start_pa = unsafe { VSCHED2_VDSO_START_PA };
        let vdso_size = unsafe { VSCHED2_VDSO_SIZE };
        let kernel_vdso_start = phys_to_virt(PhysAddr::from(vdso_start_pa)).as_usize();
        let kernel_vdso_end = kernel_vdso_start + vdso_size;
        if pos >= kernel_vdso_start && end <= kernel_vdso_end {
            return (user_vdso_base + pos - kernel_vdso_start) as *mut ();
        }

        // vVAR紧邻vDSO之前映射，使用相同的段内偏移换算。
        let vvar_start_pa = unsafe { VSCHED2_VVAR_START_PA };
        let vvar_size = unsafe { VSCHED2_VVAR_SIZE };
        let kernel_vvar_start = phys_to_virt(PhysAddr::from(vvar_start_pa)).as_usize();
        let kernel_vvar_end = kernel_vvar_start + vvar_size;
        if pos >= kernel_vvar_start && end <= kernel_vvar_end {
            let Some(user_vvar_start) = user_vdso_base.checked_sub(vvar_size) else {
                return core::ptr::null_mut();
            };
            return (user_vvar_start + pos - kernel_vvar_start) as *mut ();
        }

        // 地址C可能来自独立的共享映射，只能通过目标页表反查其用户地址B。
        aspace
            .and_then(|aspace| find_user_range(aspace, pos, len))
            .map_or(core::ptr::null_mut(), |addr| addr as *mut ())
    }
}
