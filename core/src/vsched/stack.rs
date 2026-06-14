//! VschedStackImpl — vsched2 Stack 接口实现。
//!
//! 新的 Stack trait 要求 alloc() 返回指向 Stack 实现对象的指针（非裸内存），
//! dealloc 和 base 都是该对象的方法。

use alloc::alloc::{Layout, alloc, dealloc};
use crate::config;

#[repr(C)]
pub struct VschedStackImpl {
    pub base: *mut (),
}

impl libvsched2::Stack for VschedStackImpl {
    fn alloc() -> *mut () {
        let self_layout = Layout::new::<Self>();
        let self_ptr = unsafe { alloc(self_layout) } as *mut Self;
        assert!(!self_ptr.is_null(), "VschedStackImpl: failed to alloc self");

        let stack_layout = Layout::from_size_align(config::KERNEL_STACK_SIZE, 16)
            .expect("VschedStackImpl: invalid stack layout");
        let stack_base = unsafe { alloc(stack_layout) };
        assert!(!stack_base.is_null(), "VschedStackImpl: failed to alloc stack");

        unsafe { (*self_ptr).base = stack_base as *mut (); }
        self_ptr as *mut ()
    }

    fn dealloc(&mut self) {
        let stack_layout = Layout::from_size_align(config::KERNEL_STACK_SIZE, 16)
            .expect("VschedStackImpl: invalid stack layout");
        unsafe { dealloc(self.base as *mut u8, stack_layout); }

        let self_layout = Layout::new::<Self>();
        unsafe { dealloc(self as *mut Self as *mut u8, self_layout); }
    }

    fn base(&self) -> *mut () {
        self.base
    }
}
