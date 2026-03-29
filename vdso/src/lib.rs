#![no_std]

use axhal::mem::virt_to_phys;
use axlog::{ax_println, info, warn};
use axmm::kernel_aspace;
use libvdsoexample::*;
use axalloc::{UsageKind, global_allocator};
use memory_addr::{PAGE_SIZE_4K, VirtAddr};

struct MemImpl;

#[crate_interface::impl_interface]
impl MemIf for MemImpl {
    fn alloc(size: usize) -> *mut u8 {
        let num_pages = size / PAGE_SIZE_4K;
        warn!("size: {}, num_pages: {}", size, num_pages);
        let pa = global_allocator().alloc_pages(num_pages, size, UsageKind::VirtMem).expect("vdso: alloc failed");
        let va = VirtAddr::from(pa as usize);
        let ptr = va.as_mut_ptr();
        unsafe { core::ptr::write_bytes(ptr, 0, size) };
        ptr
    }

    fn protect(addr: *mut u8, len: usize, flags: MappingFlags) {
        let vaddr = VirtAddr::from(addr as usize);
        kernel_aspace().lock().map_linear(vaddr, virt_to_phys(vaddr), len, flags).expect("vdso: protect failed");
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
    ax_println!("vDSO and vVAR loaded with the following regions:");
    for (i, (addr, size, flags)) in regions.iter().enumerate() {
        ax_println!(
            "Region {}: Address = 0x{:016x}, Size = {}, Flags = {:?}",
            i, *addr as usize, size, flags
        );
    }
    init_vtable_TestIf::<TestImpl>();
    let mut test_impl = TestImpl(10);
    let ptr = &mut test_impl as *mut TestImpl as *mut ();
    test_call(ptr);
    ax_println!("Test passed!");
}