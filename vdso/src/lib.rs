#![no_std]

use axalloc::{UsageKind, global_allocator};
use axhal::mem::{phys_to_virt, virt_to_phys};
use axhal::paging::MappingFlags;
use axlog::info;
use axmm::{AddrSpace, kernel_aspace};
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange, align_up_4k};
pub use libvsched2::*;
use libstarry_vsched::VvarData as StarryVvarData;

pub static mut VDSO_START_PA: usize = 0;
pub static mut VDSO_SIZE: usize = 0;
pub static mut VVAR_START_PA: usize = 0;
pub static mut VVAR_SIZE: usize = 0;

/// StarryOS 专用 vDSO/vVAR 在内核地址空间中的物理元数据。
pub static mut STARRY_VDSO_START_PA: usize = 0;
pub static mut STARRY_VDSO_SIZE: usize = 0;
pub static mut STARRY_VVAR_START_PA: usize = 0;
pub static mut STARRY_VVAR_SIZE: usize = 0;

const VDSO_IMAGE: &[u8] =
    include_bytes_aligned::include_bytes_aligned!(8, "../../vdso_vsched2_output/libvsched2.so");
const STARRY_VDSO_IMAGE: &[u8] = include_bytes_aligned::include_bytes_aligned!(
    8,
    "../../vdso_starry_vsched_output/libstarry_vsched.so"
);

const VVAR_RESERVED_SIZE: usize = align_up_4k(core::mem::size_of::<VvarData>());
const STARRY_VVAR_RESERVED_SIZE: usize = align_up_4k(core::mem::size_of::<StarryVvarData>());

/// Returns the page-aligned virtual span occupied by the ELF `PT_LOAD`
/// segments. `p_memsz` already includes `.bss`; file-only sections such as
/// `.symtab` and `.strtab` must not enlarge the mapped vDSO range.
fn vdso_runtime_size(image: &[u8]) -> usize {
    let elf = xmas_elf::ElfFile::new(image).expect("vdso: invalid ELF image");
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
        VDSO_SIZE = vdso_runtime_size(VDSO_IMAGE);
    };
    let vvar_kernel_expected =
        phys_to_virt(PhysAddr::from(unsafe { VDSO_START_PA })).as_usize() - unsafe { VVAR_SIZE };
    let vvar_kernel_actual =
        phys_to_virt(PhysAddr::from(unsafe { VVAR_START_PA })).as_usize();
    info!("vdso: vvar expected_kva={:#x} actual_kva={:#x} match={}",
        vvar_kernel_expected, vvar_kernel_actual,
        vvar_kernel_expected == vvar_kernel_actual);
    info!(
        "VDSO and vVAR initialized:\n  VVAR at 0x{:016x} (size: {:#x})\n  VDSO at 0x{:016x} (size: {:#x})",
        unsafe { VVAR_START_PA },
        unsafe { VVAR_SIZE },
        unsafe { VDSO_START_PA },
        unsafe { VDSO_SIZE }
    );
}

/// 在内核地址空间中首次加载 StarryOS 专用 vDSO，并初始化其 API 表。
///
/// 该模块与 vsched2 vDSO 分别分配 vVAR 和代码/数据区域；两个 loader 共用
/// `MemImpl` 的内存接口，但不会共用任何 vDSO 元数据或物理页记录。
pub fn starry_vsched_init() {
    let vdso_start = {
        let mut aspace = kernel_aspace().lock();
        let vspace = (&mut *aspace) as *mut AddrSpace as usize;
        let vdso = libstarry_vsched::map_so(vspace);
        unsafe { libstarry_vsched::init_vdso_vtable(vdso as u64) };
        // 使用固定初值验证内核与用户地址空间看到的是同一份 vVAR。
        libstarry_vsched::stage_a_set_shared(0x5354_4152_5259_5641);
        vdso
    };

    let vvar_start = unsafe { vdso_start.sub(STARRY_VVAR_RESERVED_SIZE) };
    unsafe {
        STARRY_VVAR_START_PA = usize::from(virt_to_phys(VirtAddr::from(vvar_start as usize)));
        STARRY_VVAR_SIZE = STARRY_VVAR_RESERVED_SIZE;
        STARRY_VDSO_START_PA = usize::from(virt_to_phys(VirtAddr::from(vdso_start as usize)));
        STARRY_VDSO_SIZE = vdso_runtime_size(STARRY_VDSO_IMAGE);
    };
    info!(
        "StarryOS vDSO and vVAR initialized: vVAR={:#x} size={:#x}, vDSO={:#x} size={:#x}",
        unsafe { STARRY_VVAR_START_PA },
        unsafe { STARRY_VVAR_SIZE },
        unsafe { STARRY_VDSO_START_PA },
        unsafe { STARRY_VDSO_SIZE },
    );
}

/// 返回内核地址空间中 StarryOS 专用 vVAR 的共享数据。
///
/// vVAR 物理页由 vDSO loader 分配并映射；这里通过已保存的物理地址取得内核线性映射，
/// 不依赖 vDSO 内部的代码基址扫描，也不会把该地址暴露给用户态。
pub fn starry_vvar_data() -> &'static StarryVvarData {
    let start_pa = unsafe { STARRY_VVAR_START_PA };
    assert_ne!(start_pa, 0, "StarryOS vVAR has not been initialized");
    let kva = phys_to_virt(PhysAddr::from(start_pa)).as_usize();
    unsafe { &*(kva as *const StarryVvarData) }
}

/// 将 StarryOS 专用 vDSO 映射到指定地址空间，返回 vDSO 代码基址。
pub fn map_starry_vsched_so(vspace: usize) -> usize {
    libstarry_vsched::map_so(vspace) as usize
}
