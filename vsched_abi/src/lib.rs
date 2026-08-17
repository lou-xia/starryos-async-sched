//! StarryOS 与用户态调度运行时共用的最小 vsched2 ABI 定义。
//!
//! 这里的类型只描述用户调度运行时需要观察的稳定身份和共享槽布局，不暴露
//! `AxTaskRef`、内核地址或其他内核对象。具体的 vDSO 函数仍应在出现真实消费者时，按照
//! `vdso_crate_template` 的 `extern "C"`、VTABLE 和 `#[repr(C)]` 约定加入。

#![no_std]
#![forbid(unsafe_code)]

mod process;
mod slot;
mod table;
mod task;

pub use process::*;
pub use slot::*;
pub use table::*;
pub use task::*;

#[cfg(test)]
mod tests;
