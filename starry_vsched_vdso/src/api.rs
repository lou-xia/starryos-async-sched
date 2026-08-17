use core::sync::atomic::Ordering;

use vdso_helper::get_vvar_data;
use vsched_abi::SharedTaskTable;

/// 读取阶段 A 的共享 vVAR 测试值。
#[unsafe(no_mangle)]
pub extern "C" fn stage_a_get_shared() -> usize {
    get_vvar_data!(stage_a_value).load(Ordering::Acquire)
}

/// 写入阶段 A 的共享 vVAR 测试值。
#[unsafe(no_mangle)]
pub extern "C" fn stage_a_set_shared(value: usize) {
    get_vvar_data!(stage_a_value).store(value, Ordering::Release);
}

/// 原子增加阶段 A 的共享 vVAR 测试值并返回增加前的值。
#[unsafe(no_mangle)]
pub extern "C" fn stage_a_fetch_add(value: usize) -> usize {
    get_vvar_data!(stage_a_value).fetch_add(value, Ordering::AcqRel)
}

/// 返回当前地址空间映射的共享任务表。
///
/// 表中的任务身份仍需通过 slot 和 generation 校验，返回值不能被当作内核对象指针。
#[unsafe(no_mangle)]
pub extern "C" fn user_task_table() -> *const SharedTaskTable {
    get_vvar_data!(task_table) as *const SharedTaskTable
}
