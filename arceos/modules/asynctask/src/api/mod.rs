//! 这个仓库提供与任务相关的接口，不需要使用 feature 来区分线程还是协程，
//! 无论底层的任务是以协程的形式还是以线程的形式存在，均使用这个仓库提供的接口。
//! 接口：
//! 1. current_task
//! 2. yield_now
//! 3. block_current
//! 4. exit_current
//! 5. sleep
//! 6. sleep_until
//! 7. join
//!
//! 在接口的实现层在根据不同的 feature 来调用不同的实现，
//! 但为了保证在 async 的环境下，使用 thread 类型的接口不会重复，
//! 因此还是需要根据 feature 来定义不同的行为

extern crate alloc;

mod block;
mod exit;
mod join;
mod sleep;
mod timers;
mod wait_list;
mod yield_;

pub use block::BlockFuture;
pub use exit::ExitFuture;
pub use join::JoinFuture;
pub use sleep::SleepFuture;
pub use timers::{cancel_alarm, check_events, init, set_alarm_wakeup};
pub use wait_list::{WaitTaskList, WaitWakerNode};
pub use yield_::YieldFuture;

use axhal::time::TimeValue;
use core::time::Duration;
pub use crate::*;

/// 与任务调度相关的接口都增加了返回值，这是为了保证线程与协程的兼容
/// 线程与协程的接口返回值都为 Future
/// 在 async 的环境下，两种接口都可以使用，但使用协程时，需要手动 await
/// 在线程环境下，直接使用即可
///
/// ```rust
/// // 在 async 的环境下，可以使用两种接口
/// pub async fn test_coroutine() {
///     // 使用线程的接口
///     yield_now();
///     // 使用协程的接口
///     yield_now().await;
/// }
///
/// // 在非 async 环境下，只能使用线程的接口
/// pub fn test_thread() {
///     yield_now();
/// }
///
/// ```
#[crate_interface::def_interface]
pub trait TaskApi {
    fn current_task() -> CurrentTask;

    fn yield_now() -> YieldFuture;

    fn block_current() -> BlockFuture;

    fn exit_current() -> ExitFuture;

    fn sleep(dur: Duration) -> SleepFuture;

    fn sleep_until(deadline: TimeValue) -> SleepFuture;

    fn join(task: &TaskRef) -> JoinFuture;
}

pub fn current_task() -> CurrentTask {
    crate_interface::call_interface!(TaskApi::current_task)
}

pub fn yield_now() -> YieldFuture {
    crate_interface::call_interface!(TaskApi::yield_now)
}

pub fn block_current() -> BlockFuture {
    crate_interface::call_interface!(TaskApi::block_current)
}

pub fn exit_current() -> ExitFuture {
    crate_interface::call_interface!(TaskApi::exit_current)
}

pub fn sleep(dur: Duration) -> SleepFuture {
    crate_interface::call_interface!(TaskApi::sleep, dur)
}

pub fn sleep_until(deadline: TimeValue) -> SleepFuture {
    crate_interface::call_interface!(TaskApi::sleep_until, deadline)
}

pub fn join(task: &TaskRef) -> JoinFuture {
    crate_interface::call_interface!(TaskApi::join, task)
}
