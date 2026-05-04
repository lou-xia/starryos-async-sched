//! The core functionality of a monolithic kernel, including loading user
//! programs and managing processes.

#![no_std]
#![feature(likely_unlikely)]
#![warn(missing_docs)]

extern crate alloc;

// 强制链接 libvsched2 包装库，以便将 vsched2 中 trait_interface! 生成的
// extern "C" init_vtable_* 函数纳入最终链接。core/src/vsched.rs 中通过 extern "C"
// 声明直接调用这些函数，若未显式引入则 Cargo 可能不会自动链接该依赖。
extern crate libvsched2;

#[macro_use]
extern crate axlog;

pub mod config;
pub mod futex;
pub mod mm;
pub mod resources;
pub mod shm;
pub mod task;
pub mod time;
pub mod vfs;
pub mod vsched;
