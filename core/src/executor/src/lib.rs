#![no_std]
#![feature(likely_unlikely)]

pub mod executor;
pub mod current;
pub mod api;
mod table;
mod signal;

use core::pin::Pin;

use alloc::{boxed::Box, sync::Arc};
pub use api::*;
use asynctask::Scheduler;
use kspin::SpinNoIrq;
use lazyinit::LazyInit;

use crate::executor::Executor;

extern crate axlog;
extern crate alloc;

pub type ExecutorRef = alloc::sync::Arc<Executor>;

const KERNEL_EXECUTOR_ID: usize = 1;
pub static UTRAP_HANDLER: LazyInit<fn() -> Pin<Box<dyn Future<Output = isize> + 'static>>> =
    LazyInit::new();
pub static KERNEL_SCHEDULER: LazyInit<Arc<SpinNoIrq<Scheduler>>> = LazyInit::new();
pub static KERNEL_EXECUTOR: LazyInit<Arc<Executor>> = LazyInit::new();