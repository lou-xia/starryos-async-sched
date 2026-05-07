use alloc::alloc::{Layout, alloc, dealloc};
use core::ptr::NonNull;

use crate::config;

pub struct VschedStackImpl;

impl libvsched2::Stack for VschedStackImpl {
    fn alloc() -> *mut () {
        let layout = Layout::from_size_align(config::KERNEL_STACK_SIZE, 16)
            .expect("VschedStackImpl: invalid scheduler stack layout");
        let ptr = unsafe { alloc(layout) };
        NonNull::new(ptr)
            .expect("VschedStackImpl: failed to allocate scheduler stack")
            .cast()
            .as_ptr()
    }

    fn dealloc(stack: *mut ()) {
        let layout = Layout::from_size_align(config::KERNEL_STACK_SIZE, 16)
            .expect("VschedStackImpl: invalid scheduler stack layout");
        unsafe { dealloc(stack.cast(), layout) };
    }
}
