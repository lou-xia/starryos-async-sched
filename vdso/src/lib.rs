#![no_std]

use axalloc::{UsageKind, global_allocator};
use axhal::mem::{phys_to_virt, virt_to_phys};
use axlog::ax_println;
use axmm::{AddrSpace, kernel_aspace};
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr, VirtAddrRange, align_up_4k};
pub use libvqueue::*;
// pub use libvsched2::*;

pub static mut VDSO_START_PA: usize = 0;
pub static mut VDSO_SIZE: usize = 0;
pub static mut VVAR_START_PA: usize = 0;
pub static mut VVAR_SIZE: usize = 0;

// const VDSO_IMAGE: &[u8] = include_bytes!("../../vdso_vsched2_output/libvsched2.so");
const VDSO_IMAGE: &[u8] = include_bytes!("../../vdso_vqueue_output/libvqueue.so");

const VDSO_RESERVED_SIZE: usize = align_up_4k(VDSO_IMAGE.len()) + PAGE_SIZE_4K;

const VVAR_RESERVED_SIZE: usize = align_up_4k(core::mem::size_of::<VvarData>());

struct MemImpl;

fn aspace_from_vspace(vspace: usize) -> &'static mut AddrSpace {
    assert_ne!(vspace, 0, "vdso: vspace must not be null");
    unsafe { &mut *(vspace as *mut AddrSpace) }
}

#[crate_interface::impl_interface]
impl MemIf for MemImpl {
    fn valloc(vspace: usize, size: usize) -> *mut u8 {
        let aspace = aspace_from_vspace(vspace);
        let area = VirtAddrRange::new(aspace.base(), aspace.end());
        aspace
            .find_free_area(aspace.base(), size, area, PAGE_SIZE_4K)
            .unwrap_or_else(|| {
                panic!(
                    "vdso: valloc failed: size={:#x}, aspace=[{:#x}, {:#x})",
                    size,
                    aspace.base().as_usize(),
                    aspace.end().as_usize()
                )
            })
            .as_mut_ptr()
    }

    fn ppage_alloc(size: usize) -> PhysPagePtr {
        let num_pages = size / PAGE_SIZE_4K;
        let kva = global_allocator()
            .alloc_pages(num_pages, PAGE_SIZE_4K, UsageKind::VirtMem)
            .expect("vdso: alloc physical pages failed");
        unsafe { core::ptr::write_bytes(kva as *mut u8, 0, size) };
        kva
    }

    fn map(vspace: usize, vaddr: *mut u8, ppage: PhysPagePtr, size: usize, flags: MappingFlags) {
        let aspace = aspace_from_vspace(vspace);
        aspace
            .map_linear(
                VirtAddr::from(vaddr as usize),
                virt_to_phys(VirtAddr::from(ppage)),
                size,
                flags,
            )
            .unwrap_or_else(|err| {
                panic!(
                    "vdso: map failed: err={:?}, vaddr={:#x}, size={:#x}, aspace=[{:#x}, {:#x}), \
                     flags={:?}",
                    err,
                    vaddr as usize,
                    size,
                    aspace.base().as_usize(),
                    aspace.end().as_usize(),
                    flags
                )
            });
    }

    fn change_protect(vspace: usize, vaddr: *mut u8, size: usize, flags: MappingFlags) {
        let aspace = aspace_from_vspace(vspace);
        aspace
            .protect(VirtAddr::from(vaddr as usize), size, flags)
            .expect("vdso: protect failed");
    }

    fn get_kernel_vaddr(vspace: usize, vaddr: *mut u8) -> *mut u8 {
        let aspace = aspace_from_vspace(vspace);
        let addr = VirtAddr::from(vaddr as usize);
        let page_base = addr.align_down_4k();
        let offset = addr.align_offset_4k();
        let (paddr, ..) = aspace
            .page_table()
            .query(page_base)
            .expect("vdso: query page table failed");
        phys_to_virt(paddr + offset).as_mut_ptr()
    }

    fn ppage_clone(ppage: PhysPagePtr) -> PhysPagePtr {
        ppage
    }
}

pub fn vdso_init() {
    // 首次加载到内核地址空间，同时初始化内核侧的 vDSO 函数表。
    let vdso_start = {
        let mut aspace = kernel_aspace().lock();
        let vspace = (&mut *aspace) as *mut AddrSpace as usize;
        let vdso = map_so(vspace);
        unsafe { init_vdso_vtable(vdso as u64) };
        vdso
    };

    let vvar_start = unsafe { vdso_start.sub(VVAR_RESERVED_SIZE) };
    unsafe {
        VVAR_START_PA = usize::from(virt_to_phys(VirtAddr::from(vvar_start as usize)));
        VVAR_SIZE = VVAR_RESERVED_SIZE;
        VDSO_START_PA = usize::from(virt_to_phys(VirtAddr::from(vdso_start as usize)));
        VDSO_SIZE = VDSO_RESERVED_SIZE;
    };
    ax_println!(
        "VDSO and vVAR initialized:\n  VVAR at 0x{:016x} (size: {:#x})\n  VDSO at 0x{:016x} (size: {:#x})",
        unsafe { VVAR_START_PA },
        unsafe { VVAR_SIZE },
        unsafe { VDSO_START_PA },
        unsafe { VDSO_SIZE }
    );
}
