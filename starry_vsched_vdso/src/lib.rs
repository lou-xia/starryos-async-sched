//! StarryOS 专用 vDSO。
//!
//! 提供 StarryOS 用户态调度所需的共享任务表和用户侧 ABI。共享任务表保存
//! 跨地址空间可见的任务投影，具体调度策略仍由 vsched2 实现。

#![no_std]

mod api;

use core::sync::atomic::AtomicUsize;
use vdso_helper::vvar_data;
pub use vsched_abi::SharedTaskTable;

pub use api::*;

// 阶段 A 用于验证 vVAR 共享语义的全局计数器。
vvar_data! {
    stage_a_value: AtomicUsize,
    /// 共享任务槽表。内核 registry 才是内核任务对象的权威来源。
    task_table: SharedTaskTable,
}
