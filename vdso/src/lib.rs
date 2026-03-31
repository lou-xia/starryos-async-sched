#![no_std]

use axalloc::{UsageKind, global_allocator};
use axhal::mem::virt_to_phys;
use axlog::{ax_println, info};
use axmm::kernel_aspace;
pub use libvdsoexample::*;
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr, align_up_4k};

pub static mut VDSO_START: usize = 0;
pub static mut VDSO_SIZE: usize = 0;
pub static mut VVAR_START: usize = 0;
pub static mut VVAR_SIZE: usize = 0;

struct MemImpl;

#[crate_interface::impl_interface]
impl MemIf for MemImpl {
    fn alloc(size: usize) -> *mut u8 {
        let num_pages = size / PAGE_SIZE_4K;
        let pa = global_allocator()
            .alloc_pages(num_pages, 0x1000, UsageKind::VirtMem)
            .expect("vdso: alloc failed");
        let va = VirtAddr::from(pa as usize);
        let ptr = va.as_mut_ptr();
        unsafe { core::ptr::write_bytes(ptr, 0, size) };
        ptr
    }

    fn protect(addr: *mut u8, len: usize, flags: MappingFlags) {
        let vaddr = VirtAddr::from(addr as usize);
        let len = align_up_4k(len);
        let mut new_flags = flags.clone();
        if flags.contains(MappingFlags::USER) {
            new_flags.remove(MappingFlags::USER);
        }
        kernel_aspace()
            .lock()
            .protect(vaddr, len, new_flags)
            .expect("vdso: protect failed");
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
    let regions = load_and_init();
    unsafe {
        VVAR_START = usize::from(virt_to_phys(VirtAddr::from_usize(regions[0].0 as usize)));
        VVAR_SIZE = regions[0].1.align_up_4k();
        VDSO_START = usize::from(virt_to_phys(VirtAddr::from_usize(regions[1].0 as usize)));
        VDSO_SIZE = regions.iter().skip(1).map(|(_, size, _)| *size).sum::<usize>().align_up_4k();
    };
    ax_println!("VDSO and vVAR initialized: VVAR at 0x{:016x} (size: {}), VDSO at 0x{:016x} (size: {})", unsafe { VVAR_START }, unsafe { VVAR_SIZE }, unsafe { VDSO_START }, unsafe { VDSO_SIZE });
    ax_println!("vDSO and vVAR loaded with the following regions:");
    for (i, (addr, size, flags)) in regions.iter().enumerate() {
        ax_println!(
            "Region {}: Address = 0x{:016x}, Size = {}, Flags = {:?}",
            i,
            *addr as usize,
            size,
            flags
        );
    }
    init_vtable_TestIf::<TestImpl>();
    let mut test_impl = TestImpl(10);
    let ptr = &mut test_impl as *mut TestImpl as *mut ();
    test_call(ptr);
    ax_println!("Test passed!");
}
