#![no_std]

use axalloc::{UsageKind, global_allocator};
use axhal::mem::{phys_to_virt, virt_to_phys};
use axhal::paging::MappingFlags;
use axlog::ax_println;
use axmm::{AddrSpace, kernel_aspace};
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange, align_up_4k};
pub use libvsched2::*;

pub static mut VDSO_START_PA: usize = 0;
pub static mut VDSO_SIZE: usize = 0;
pub static mut VVAR_START_PA: usize = 0;
pub static mut VVAR_SIZE: usize = 0;

const VDSO_IMAGE: &[u8] =
    include_bytes_aligned::include_bytes_aligned!(8, "../../vdso_vsched2_output/libvsched2.so");

const VVAR_RESERVED_SIZE: usize = align_up_4k(core::mem::size_of::<VvarData>());

/// Returns the page-aligned virtual span occupied by the ELF `PT_LOAD`
/// segments. `p_memsz` already includes `.bss`; file-only sections such as
/// `.symtab` and `.strtab` must not enlarge the mapped vDSO range.
fn vdso_runtime_size() -> usize {
    let elf = xmas_elf::ElfFile::new(VDSO_IMAGE).expect("vdso: invalid ELF image");
    let mut runtime_start = usize::MAX;
    let mut runtime_end = 0usize;

    for ph in elf
        .program_iter()
        .filter(|ph| ph.get_type() == Ok(xmas_elf::program::Type::Load))
    {
        let start = (ph.virtual_addr() as usize) & !(PAGE_SIZE_4K - 1);
        let end = align_up_4k((ph.virtual_addr() + ph.mem_size()) as usize);
        runtime_start = runtime_start.min(start);
        runtime_end = runtime_end.max(end);
    }

    assert_ne!(runtime_start, usize::MAX, "vdso: ELF has no PT_LOAD segment");
    // The generated loader treats map_so()'s return value as the ELF load
    // bias, so the loadable ET_DYN image is required to begin at offset zero.
    assert_eq!(runtime_start, 0, "vdso: PT_LOAD span must start at offset zero");
    runtime_end - runtime_start
}

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
        // Keep StarryOS's placement decision consistent with the ELF runtime
        // span even if a generated loader computes its request differently.
        let size = size.max(VVAR_RESERVED_SIZE + vdso_runtime_size());
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

    fn map(vspace: usize, vaddr: *mut u8, ppage: PhysPagePtr, size: usize, flags: MappingFlags, _shared: bool) {
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
        VDSO_SIZE = vdso_runtime_size();
    };
    let vvar_kernel_expected =
        phys_to_virt(PhysAddr::from(unsafe { VDSO_START_PA })).as_usize() - unsafe { VVAR_SIZE };
    let vvar_kernel_actual =
        phys_to_virt(PhysAddr::from(unsafe { VVAR_START_PA })).as_usize();
    ax_println!("vdso: vvar expected_kva={:#x} actual_kva={:#x} match={}",
        vvar_kernel_expected, vvar_kernel_actual,
        vvar_kernel_expected == vvar_kernel_actual);
    ax_println!(
        "VDSO and vVAR initialized:\n  VVAR at 0x{:016x} (size: {:#x})\n  VDSO at 0x{:016x} (size: {:#x})",
        unsafe { VVAR_START_PA },
        unsafe { VVAR_SIZE },
        unsafe { VDSO_START_PA },
        unsafe { VDSO_SIZE }
    );
}

// raw_trap_entry: 24c6; trap_entry: 4310
