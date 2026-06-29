//! 通过vDSO访问的API，可以操作全局数据结构
use core::mem;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{ARRAY_LEN, IPCItem, LockFreeDeque, PerProcess, QUEUE_CAPACITY, SlotGuard, SlotRef};

use crate::get_queue_array;

/// 注册当前进程，返回一个`SlotRef`，其中包含了当前进程的IPC数据结构。
#[unsafe(no_mangle)]
pub extern "C" fn register_process() -> Result<SlotRef<'static, PerProcess, ARRAY_LEN>, ()> {
    get_queue_array().push(PerProcess::default())
}

/// 向当前进程的IPC队列（`deque`）中推入一条消息。
#[unsafe(no_mangle)]
pub extern "C" fn deque_push(process_id: usize, item: IPCItem) -> Result<(), IPCItem> {
    let slot_ref: SlotRef<'_, PerProcess, ARRAY_LEN> = unsafe { SlotRef::from_id(process_id) };
    let res = slot_ref.deque.push_front(item);
    slot_ref.into_id(); // prevent drop
    res
}

// // Don't work because of lifetime issue
// #[unsafe(no_mangle)]
// pub extern "C" fn push_slot(queue_id: usize) -> Result<SlotGuard<'static, IPCItem>, ()> {
//     let slot_ref: SlotRef<'static, PerProcess, ARRAY_LEN> =
//         unsafe { SlotRef::from_id(queue_id) };
//     let res: Result<SlotGuard<'static, IPCItem>, ()> = slot_ref.push_slot_front();
//     slot_ref.into_id(); // prevent drop
//     res
// }

/// 从当前进程的IPC队列（`deque`）中弹出一条消息。
#[unsafe(no_mangle)]
pub extern "C" fn deque_pop(process_id: usize) -> Option<IPCItem> {
    let slot_ref: SlotRef<'_, PerProcess, ARRAY_LEN> = unsafe { SlotRef::from_id(process_id) };
    let res = slot_ref.deque.pop_back();
    slot_ref.into_id(); // prevent drop
    res
}

/// 从进程id获取对应的`SlotRef`，以操作`SlotRef`。
///
/// 当前，该接口只用于clone。
///
/// # Safety
///
/// The caller must ensure that the id is get from `SlotRef::into_id`.
///
/// one id can only be converted back to one `SlotRef`.
#[unsafe(no_mangle)]
pub extern "C" fn slotref_from_id(process_id: usize) -> SlotRef<'static, PerProcess, ARRAY_LEN> {
    unsafe { SlotRef::from_id(process_id) }
}

/// 获取先前使用`set_pid`设置的`pid`。
///
/// - `process_id`：使用`register_process`分配的pid
/// - `pid`：调度模块中的进程id，用于通知机制
#[unsafe(no_mangle)]
pub extern "C" fn get_pid(process_id: usize) -> usize {
    let slot_ref: SlotRef<'_, PerProcess, ARRAY_LEN> = unsafe { SlotRef::from_id(process_id) };
    let res = slot_ref.pid.load(Ordering::Acquire);
    slot_ref.into_id(); // prevent drop
    res
}

/// 记录当前进程的`pid`。
///
/// 一些通知操作需要使用`get_pid`获取当前进程的`pid`，因此在此之前需要先调用该函数设置pid。
///
/// - `process_id`：使用`register_process`分配的pid
/// - `pid`：调度模块中的进程id，用于通知机制
#[unsafe(no_mangle)]
pub extern "C" fn set_pid(process_id: usize, pid: usize) {
    let slot_ref: SlotRef<'_, PerProcess, ARRAY_LEN> = unsafe { SlotRef::from_id(process_id) };
    slot_ref.pid.store(pid, Ordering::Release);
    slot_ref.into_id(); // prevent drop
}

/// 添加从msg_type（调度器协程id）到ntf_id（通知源id）的映射
#[unsafe(no_mangle)]
pub extern "C" fn map_add_entry(
    process_id: usize,
    msg_type: usize,
    ntf_id: usize,
) -> Result<(), ()> {
    let slot_ref: SlotRef<'_, PerProcess, ARRAY_LEN> = unsafe { SlotRef::from_id(process_id) };
    let res = slot_ref.map.push((msg_type, ntf_id));
    let res = res.map(|sref| {
        mem::forget(sref); // 保持引用计数
    });
    slot_ref.into_id(); // prevent drop
    res
}

/// 根据msg_type（调度器协程id）查找ntf_id（通知源id）
#[unsafe(no_mangle)]
pub extern "C" fn map_get_ntf_id(process_id: usize, msg_type: usize) -> Option<usize> {
    let slot_ref: SlotRef<'_, PerProcess, ARRAY_LEN> = unsafe { SlotRef::from_id(process_id) };
    for i in 0..ARRAY_LEN {
        if let Some(&(this_msg_type, this_ntf_id)) = slot_ref.map.get(i) {
            if this_msg_type == msg_type || this_msg_type == usize::MAX {
                slot_ref.into_id(); // prevent drop
                return Some(this_ntf_id);
            }
        }
    }
    slot_ref.into_id(); // prevent drop
    None
}

/// 删除从msg_type（调度器协程id）到ntf_id（通知源id）的映射
#[unsafe(no_mangle)]
pub extern "C" fn map_pop_ntf_id(process_id: usize, msg_type: usize) -> Option<usize> {
    let slot_ref: SlotRef<'_, PerProcess, ARRAY_LEN> = unsafe { SlotRef::from_id(process_id) };
    for i in 0..ARRAY_LEN {
        if let Some(&(this_msg_type, this_ntf_id)) = slot_ref.map.get(i) {
            if this_msg_type == msg_type {
                // 删除slot
                unsafe {
                    slot_ref.map.drop_slot(i);
                }
                slot_ref.into_id(); // prevent drop
                return Some(this_ntf_id);
            }
        }
    }
    slot_ref.into_id(); // prevent drop
    None
}
