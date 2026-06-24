//! VschedStackImpl — vsched2 Stack 接口实现。
//!
//! 布局: [VschedStackImpl: 16B][buffer: KERNEL_STACK_SIZE bytes]
//!                                               ↑ base 指向 buffer 顶部(高地址)
//!
//! 栈从 buffer 顶部向下增长, 确保 Rust 函数序言 (addi sp,sp,-N)
//! 永远不会侵入 VSI 区域, 彻底避免了连续分配下的自覆盖问题。
//!
//! TODO: trap 向量入口的偏移量需要从正向(sp+offset)改为负向(sp-offset),
//!       因为 sscratch 现在指向 buffer 顶部而非底部。

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

pub fn vsi_from_base(base: *mut ()) -> *mut VschedStackImpl {
    // base = buffer_top = ptr + sizeof(VSI) + KERNEL_STACK_SIZE
    // VSI = base - KERNEL_STACK_SIZE - sizeof(VSI)
    //     = base - (KERNEL_STACK_SIZE + vsi_layout.size())
    let vsi_layout = Layout::new::<VschedStackImpl>();
    let vsi_addr = (base as usize) - config::KERNEL_STACK_SIZE - vsi_layout.size();
    vsi_addr as *mut VschedStackImpl
}

impl libvsched2::Stack for VschedStackImpl {
    fn alloc() -> *mut () {
        let (total_layout, stack_offset) = stack_allocation_layout();
        let ptr = unsafe { alloc(total_layout) };
        assert!(!ptr.is_null(), "VschedStackImpl: failed to alloc");

        let self_ptr = ptr as *mut Self;
        // base points to the TOP of the buffer (high address), so stack grows
        // downwards AWAY from the VSI struct at the low end.
        let buffer_top = unsafe { ptr.add(stack_offset).add(config::KERNEL_STACK_SIZE) };
        unsafe {
            (*self_ptr).magic = VSI_MAGIC;
            (*self_ptr).base = buffer_top as *mut ();
        }
        axlog::ax_println!(
            "[stack::alloc] vsi={:#x} magic={:#x} base={:#x} (buffer_top) layout_size={}",
            self_ptr as usize, VSI_MAGIC, buffer_top as usize, total_layout.size(),
        );
        self_ptr as *mut ()
    }

    fn dealloc(&mut self) {
        let m = self.magic;
        let b = self.base;
        if m != VSI_MAGIC {
            axlog::ax_println!(
                "[stack::dealloc] CORRUPT! vsi={:#x} magic={:#x} (expected {:#x}) base={:#x}",
                self as *mut Self as usize, m, VSI_MAGIC, b as usize,
            );
        }
        let (total_layout, _) = stack_allocation_layout();
        let ptr = self as *mut Self as *mut u8;
        unsafe { dealloc(ptr, total_layout); }
    }

    fn base(&self) -> *mut () {
        let addr = self as *const Self as usize;
        let m = self.magic;
        let b = self.base;
        if m != VSI_MAGIC {
            axlog::ax_println!(
                "[stack::base] CORRUPT! this={:#x} magic={:#x} (expected {:#x}) base={:#x}",
                addr, m, VSI_MAGIC, b as usize,
            );
        } else {
            axlog::ax_println!("[stack::base] this={:#x} magic=OK base={:#x}", addr, b as usize);
        }
        b
    }

    fn from_base(base: *mut ()) -> *mut Self {
        let pre_save_base = crate::vsched::PRE_SAVE_BASE.load(core::sync::atomic::Ordering::Acquire);
        let pre_save_top = crate::vsched::PRE_SAVE_TOP.load(core::sync::atomic::Ordering::Acquire);
        if base.is_null()
            || base as usize == pre_save_base
            || base as usize == pre_save_top  // raw pre-save stack top (sscratch = stack_top)
        {
            axlog::ax_println!("[stack::from_base] base={:#x} SKIP (raw pre-save)", base as usize);
            return core::ptr::null_mut();
        }
        let vsi = vsi_from_base(base);
        unsafe {
            let m = (*vsi).magic;
            if m != VSI_MAGIC {
                axlog::ax_println!(
                    "[stack::from_base] CORRUPT! base={:#x} vsi={:#x} magic={:#x} (expected {:#x})",
                    base as usize, vsi as usize, m, VSI_MAGIC,
                );
            }
        }
        vsi
    }
}
