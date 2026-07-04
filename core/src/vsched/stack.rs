//! VschedStackImpl — vsched2 Stack 接口实现。
//!
//! 布局: [VschedStackImpl: 16B][buffer: KERNEL_STACK_SIZE bytes]
//!                                               ↑ base 指向 buffer 顶部(高地址)
//!
//! 栈从 buffer 顶部向下增长, 确保 Rust 函数序言 (addi sp,sp,-N)
//! 永远不会侵入 VSI 区域, 彻底避免了连续分配下的自覆盖问题。

use alloc::alloc::{Layout, alloc, dealloc};

use crate::config;

#[repr(C)]
pub struct VschedStackImpl {
    pub base: *mut (),
    pub magic: u64,
}

const VSI_MAGIC: u64 = 0xdeadbeefcafebabe;

fn stack_allocation_layout() -> (Layout, usize) {
    let vsi_layout = Layout::new::<VschedStackImpl>();
    let stack_layout = Layout::from_size_align(config::KERNEL_STACK_SIZE, 16)
        .expect("VschedStackImpl: invalid stack layout");
    vsi_layout
        .extend(stack_layout)
        .expect("VschedStackImpl: layout extend failed")
}

impl libvsched2::Stack for VschedStackImpl {
    fn alloc() -> *mut () {
        let (total_layout, stack_offset) = stack_allocation_layout();
        let ptr = unsafe { alloc(total_layout) };
        assert!(!ptr.is_null(), "VschedStackImpl: failed to alloc");

        let self_ptr = ptr as *mut Self;
        let buffer_top = unsafe { ptr.add(stack_offset).add(config::KERNEL_STACK_SIZE) };
        unsafe {
            (*self_ptr).magic = VSI_MAGIC;
            (*self_ptr).base = buffer_top as *mut ();
        }
        axlog::ax_println!(
            "[stack::alloc] vsi={:#x} base={:#x}",
            self_ptr as usize, buffer_top as usize,
        );
        self_ptr as *mut ()
    }

    fn dealloc(&mut self) {
        let (total_layout, _) = stack_allocation_layout();
        let ptr = self as *mut Self as *mut u8;
        unsafe { dealloc(ptr, total_layout); }
    }

    fn base(&self) -> *mut () {
        assert_eq!(
            self.magic, VSI_MAGIC,
            "VschedStackImpl::base: CORRUPT magic at {:#x} (magic={:#x})",
            self as *const Self as usize, self.magic,
        );
        assert!(
            !self.base.is_null(),
            "VschedStackImpl::base: NULL base at {:#x}",
            self as *const Self as usize,
        );
        self.base
    }
}
