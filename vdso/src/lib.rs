#![no_std]

use axalloc::{UsageKind, global_allocator};
use axhal::mem::{phys_to_virt, virt_to_phys};
use axlog::{ax_println, info};
use axmm::{AddrSpace, kernel_aspace};
pub use libvdsoexample::*;
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr, VirtAddrRange, align_up_4k};
use vdso_example::VvarData as LocalVvarData;

pub static mut VDSO_START_PA: usize = 0;
pub static mut VDSO_SIZE: usize = 0;
pub static mut VVAR_START_PA: usize = 0;
pub static mut VVAR_SIZE: usize = 0;

struct MemImpl;

/// 生成出的 vDSO 镜像文件。
const VDSO_IMAGE: &[u8] = include_bytes!("../../vdso_output/libvdsoexample.so");

/// 生成器会额外为未出现在文件中的段预留一页，因此这里和 loader 中的计算保持一致。
const VDSO_RESERVED_SIZE: usize = align_up_4k(VDSO_IMAGE.len()) + PAGE_SIZE_4K;

/// vVAR 区只需要容纳导出的共享数据结构。
const VVAR_RESERVED_SIZE: usize = align_up_4k(core::mem::size_of::<LocalVvarData>());

/// `build_vdso` 新接口使用裸 `usize` 传递目标地址空间，这里统一恢复为 `AddrSpace`。
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

    /// 当前实现用一段连续内核虚拟地址承载物理页，并把该虚拟地址本身作为 `PhysPagePtr`。
    /// 后续 `map()` 时再将其还原为物理地址，这样无需修改生成器接口即可复用现有页分配器。
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

struct TestImpl(usize);

impl TestIf for TestImpl {
    fn test_fn1(&self, arg: usize) -> usize {
        info!("test_fn1 called with arg: {}, self.0: {}", arg, self.0);
        self.0 + arg
    }

    fn test_fn2(&mut self, arg: usize) -> usize {
        info!("test_fn2 called with arg: {}, self.0: {}", arg, self.0);
        self.0 += arg;
        self.0
    }

    fn test_fn3(arg: usize) {
        info!("test_fn3 called with arg: {}", arg);
    }
}

pub fn vdso_init() {
    info!("Starting VDSO test...");
    // 首次加载到内核地址空间，同时初始化内核侧的 vDSO 函数表。
    let vdso_start = {
        let mut aspace = kernel_aspace().lock();
        let vspace = (&mut *aspace) as *mut AddrSpace as usize;
        let vdso = map_so(vspace);
        unsafe { init_vdso_vtable(vdso as _) };
        vdso
    };

    // loader 约定 vVAR 紧邻在 vDSO 前面，因此这里按同一布局回填导出给其余模块使用的元数据。
    let vvar_start = unsafe { vdso_start.sub(VVAR_RESERVED_SIZE) };
    unsafe {
        VVAR_START_PA = usize::from(virt_to_phys(VirtAddr::from(vvar_start as usize)));
        VVAR_SIZE = VVAR_RESERVED_SIZE;
        VDSO_START_PA = usize::from(virt_to_phys(VirtAddr::from(vdso_start as usize)));
        VDSO_SIZE = VDSO_RESERVED_SIZE;
    };
    ax_println!(
        "VDSO and vVAR initialized: VVAR at 0x{:016x} (size: {}), VDSO at 0x{:016x} (size: {})",
        unsafe { VVAR_START_PA },
        unsafe { VVAR_SIZE },
        unsafe { VDSO_START_PA },
        unsafe { VDSO_SIZE }
    );
    init_vtable_TestIf::<TestImpl>();
    let mut test_impl = TestImpl(10);
    let ptr = &mut test_impl as *mut TestImpl as *mut ();
    test_call(ptr);
    ax_println!("Test passed!");
    set_shared(100);
    set_private(200);
}
