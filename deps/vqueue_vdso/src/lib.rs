//! 由vDSO管理的IPC队列等数据结构，通过vDSO实现了进程间共享。
//! 其为每个进程存储了IPC队列、进程id，以及从调度器协程到通知源id的映射，用于实现IPC的消息传递与通知功能。

#![no_std]
#![deny(missing_docs)]

use core::sync::atomic::{AtomicU64, AtomicUsize};
#[cfg(not(feature = "vdso"))]
use core::{mem::MaybeUninit, ptr::NonNull, sync::atomic::AtomicPtr};

#[cfg(not(feature = "vdso"))]
use lazyinit::LazyInit;

use crate::slot_array::SlotArray;

mod api;
pub use api::*;
mod deque;
pub use deque::{LockFreeDeque, SlotGuard};
mod ipc_item;
pub use ipc_item::IPCItem;
mod slot_array;
pub use slot_array::SlotRef;

vdso_helper::use_mut_cfg! {}
/// 队列占用的空间，为队列长度加1，以区分满和空的情况
pub const QUEUE_CAPACITY: usize = QUEUE_LEN + 1;

/// 每个进程的IPC数据结构
#[derive(Default)]
pub struct PerProcess {
    /// IPC消息的接收队列
    deque: LockFreeDeque<IPCItem, QUEUE_CAPACITY>,
    /// 进程id，用于通知机制
    pid: AtomicUsize,
    /// 从msg_type（调度器协程id）到ntf_id（通知源id，例如信号编号）的映射
    ///
    /// 若登记的msg_type为USIZE_MAX，则查找时视为任何msg_type均对应到这一项
    map: SlotArray<(usize, usize), ARRAY_LEN>,
}

// 存放于vDSO中的全局数据结构，包含每个进程的IPC数据结构数组
#[cfg(feature = "vdso")]
vdso_helper::vvar_data! {
    queue_array: SlotArray<PerProcess, ARRAY_LEN>,
}

#[cfg(not(feature = "vdso"))]
/// 存储队列数组地址的全局变量
static QUEUE_ARRAY_ADDR: LazyInit<usize> = LazyInit::new();

#[cfg(not(feature = "vdso"))]
pub const QUEUE_ARRAY_SIZE: usize = core::mem::size_of::<SlotArray<PerProcess, ARRAY_LEN>>();

/// Set the address of the queue array.
///
/// # Safety
///
/// The address must refer to a `SlotArray<PerProcess, ARRAY_LEN>` that is already initialized,
/// and be valid for the lifetime of the program.
///
/// Before calling other functions, `set_queue_array_addr` or `set_queue_array_addr_and_init`
/// must be called once and only once.
#[cfg(not(feature = "vdso"))]
pub unsafe fn set_queue_array_addr(addr: NonNull<()>) {
    QUEUE_ARRAY_ADDR.init_once(addr.as_ptr() as usize);
}

#[cfg(not(feature = "vdso"))]
/// Initialize the queue array at the given address.
///
/// # Safety
///
/// The address must be valid for the lifetime of the program.
///
/// Before calling other functions, `set_queue_array_addr` or `set_queue_array_addr_and_init`
/// must be called once and only once.
pub unsafe fn set_queue_array_addr_and_init(addr: NonNull<()>) {
    QUEUE_ARRAY_ADDR.init_once(addr.as_ptr() as usize);
    unsafe {
        ((*QUEUE_ARRAY_ADDR.get().unwrap()) as *mut () as *mut SlotArray<PerProcess, ARRAY_LEN>)
            .write(SlotArray::new())
    };
}

pub(crate) fn get_queue_array() -> &'static SlotArray<PerProcess, ARRAY_LEN> {
    #[cfg(feature = "vdso")]
    {
        vdso_helper::get_vvar_data! {
            queue_array
        }
    }
    #[cfg(not(feature = "vdso"))]
    {
        unsafe {
            &*((*QUEUE_ARRAY_ADDR.get().expect(
                "QUEUE_ARRAY_ADDR is not initialized. Please call `set_queue_array_addr` or `set_queue_array_addr_and_init` first.",
            )) as *const ()
                as *const SlotArray<PerProcess, ARRAY_LEN>)
        }
    }
}

#[cfg(test)]
mod test_mut_cfg {
    extern crate std;

    use super::{ARRAY_LEN, QUEUE_LEN};
    use std::println;

    // run with `cargo test test_constants -- --nocapture`
    #[test]
    fn test_constants() {
        println!("QUEUE_LEN: {}", QUEUE_LEN);
        println!("ARRAY_LEN: {}", ARRAY_LEN);
        // println!("BOOL_TEST: {}", BOOL_TEST);
        // println!("EXPR_TEST: {}", EXPR_TEST);
        // println!("FLOAT_TEST: {}", FLOAT_TEST);
    }
}
