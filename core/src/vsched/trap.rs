use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::task::Poll;

use axsync::Mutex;
use axtask::TaskState as AxTaskState;
use lazy_static::lazy_static;

use crate::config;

use super::task::{CoroutinePoll, VschedTaskImpl};
use super::{register_task, to_vsched_state, HIGHEST_PRIORITY};

// Trap dispatcher 由 starry-api 在 init 时注册，桥接 core→api 的依赖方向。
type TrapDispatcher = fn(trapped_task: *const VschedTaskImpl);

static TRAP_DISPATCHER: AtomicUsize = AtomicUsize::new(0);

pub fn register_trap_dispatcher(dispatcher: TrapDispatcher) {
    TRAP_DISPATCHER.store(dispatcher as usize, Ordering::Release);
}

// ---- Trap handler pool ----

/// 池上限。Blocked 状态用完时可动态扩到该值，再满则 panic。
const TRAP_HANDLER_POOL_CAP: usize = 8;

lazy_static! {
    /// 存储池中 handler 的裸指针（usize 避 Send 问题）。
    static ref TRAP_HANDLER_POOL: Mutex<Vec<usize>> = Mutex::new(Vec::new());
}

/// 每个 handler 对应的协程。vsched2 调度到该 handler 时会调用 `poll()`。
///
/// `trapped_task` 由 `get_handler` 写入，`poll` 以 swap 读出并清空，
/// 随后交 `TRAP_DISPATCHER` 分派处理。
struct TrapHandlerCoroutine {
    trapped_task: AtomicUsize,
}

impl CoroutinePoll for TrapHandlerCoroutine {
    fn poll(&self) -> Poll<usize> {
        // swap 读取并立即清零，避免同一 handler 被重复调度时读到旧 task
        let trapped_task = self.trapped_task.swap(0, Ordering::AcqRel) as *const VschedTaskImpl;
        if trapped_task.is_null() {
            return Poll::Ready(0);
        }
        let dispatcher = TRAP_DISPATCHER.load(Ordering::Acquire);
        if dispatcher != 0 {
            let dispatcher: TrapDispatcher = unsafe { core::mem::transmute(dispatcher) };
            dispatcher(trapped_task);
        }
        Poll::Ready(0)
    }
}

// ---- Handler 生命周期 ----

/// 创建一个新的 trap handler 任务。
///
/// 底层 `axtask` 为 Blocked 态，不会被 axtask 调度器执行；
/// vsched2 接管后会将其作为协程运行 `poll()`。
///
/// axtask 闭包为空：handler 始终通过协程 poll 执行，不会走线程入口。
fn create_trap_handler() -> usize {
    let task_ref = axtask::spawn_raw(
        || {},
        alloc::string::String::from("trap_handler"),
        config::KERNEL_STACK_SIZE,
    );
    task_ref.set_state(AxTaskState::Blocked);
    let coro = Arc::new(TrapHandlerCoroutine {
        trapped_task: AtomicUsize::new(0),
    });
    let ptr = register_task(task_ref, HIGHEST_PRIORITY, 0, Some(coro));
    ptr as usize
}

/// 预分配 `TRAP_HANDLER_POOL_CAP` 个 handler 填入池中。
pub fn init_trap_handler_pool() {
    let mut pool = TRAP_HANDLER_POOL.lock();
    for _ in 0..TRAP_HANDLER_POOL_CAP {
        pool.push(create_trap_handler());
    }
}

// ---- VschedTrapHandleImpl ----

pub struct VschedTrapHandleImpl;

impl libvsched2::TrapHandle for VschedTrapHandleImpl {
    /// 获取一个 Blocked 状态的 trap handler。
    ///
    /// vsched2 的 `trap_handle()` 在同步 trap 发生时调用本函数，传入被 trap
    /// 的任务指针。返回值作为 handler 任务交给 vsched2 调度执行。
    ///
    /// 优先从池中取现成的 Blocked handler；池空则创建新 handler。
    fn get_handler(task: *const ()) -> *const () {
        /// 将 trapped task 指针写入 handler 内部的 `TrapHandlerCoroutine`。
        ///
        /// handler.coroutine 是 `Arc<dyn CoroutinePoll>`，其 concrete type
        /// 为 `TrapHandlerCoroutine`。通过 `Arc::as_ptr` 获取数据指针后
        /// 裸指针转换即可访问 `trapped_task` 字段。
        fn set_trapped_task(handler: &VschedTaskImpl, task: *const ()) {
            if let Some(coro) = &handler.coroutine {
                let coro = unsafe { &*(Arc::as_ptr(coro) as *const TrapHandlerCoroutine) };
                coro.trapped_task.store(task as usize, Ordering::Release);
            }
        }

        let mut pool = TRAP_HANDLER_POOL.lock();
        // 遍历池中找 Blocked handler
        if let Some(pos) = pool.iter().position(|&ptr| {
            let handler = unsafe { &*(ptr as *const VschedTaskImpl) };
            matches!(
                to_vsched_state(handler.task.state()),
                libvsched2::TaskState::Blocked
            )
        }) {
            let ptr = pool[pos];
            let handler = unsafe { &*(ptr as *const VschedTaskImpl) };
            set_trapped_task(handler, task);
            handler.task.set_state(AxTaskState::Ready);
            ptr as *const ()
        } else {
            // 池空：检查上限后创建新 handler
            if pool.len() >= TRAP_HANDLER_POOL_CAP {
                panic!("Trap handler pool exhausted ({} handlers all busy)", pool.len());
            }
            let new_handler = create_trap_handler();
            pool.push(new_handler);
            let handler = unsafe { &*(new_handler as *const VschedTaskImpl) };
            set_trapped_task(handler, task);
            handler.task.set_state(AxTaskState::Ready);
            new_handler as *const ()
        }
    }
}
