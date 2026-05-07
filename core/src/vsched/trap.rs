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

const TRAP_HANDLER_POOL_CAP: usize = 10;

lazy_static! {
    static ref TRAP_HANDLER_POOL: Mutex<Vec<usize>> = Mutex::new(Vec::new());
}

struct TrapHandlerCoroutine {
    trapped_task: AtomicUsize,
}

impl CoroutinePoll for TrapHandlerCoroutine {
    fn poll(&self) -> Poll<usize> {
        let _trapped_task = self.trapped_task.swap(0, Ordering::AcqRel) as *const ();
        // TODO
        Poll::Ready(0)
    }
}

fn create_trap_handler() -> usize {
    let task_ref = axtask::spawn_raw(
        || {
            // TODO
        },
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

pub fn init_trap_handler_pool() {
    let mut pool = TRAP_HANDLER_POOL.lock();
    for _ in 0..TRAP_HANDLER_POOL_CAP {
        pool.push(create_trap_handler());
    }
}

pub struct VschedTrapHandleImpl;

impl libvsched2::TrapHandle for VschedTrapHandleImpl {
    fn get_handler(task: *const ()) -> *const () {
        let mut pool = TRAP_HANDLER_POOL.lock();
        if let Some(pos) = pool.iter().position(|&ptr| {
            let handler = unsafe { &*(ptr as *const VschedTaskImpl) };
            matches!(
                to_vsched_state(handler.task.state()),
                libvsched2::TaskState::Blocked
            )
        }) {
            let ptr = pool[pos];
            let handler = unsafe { &*(ptr as *const VschedTaskImpl) };
            handler.task.set_state(AxTaskState::Ready);
            ptr as *const ()
        } else {
            if pool.len() >= TRAP_HANDLER_POOL_CAP {
                panic!("Trap handler pool exhausted ({} handlers all busy)", pool.len());
            }
            let new_handler = create_trap_handler();
            pool.push(new_handler);
            let handler = unsafe { &*(new_handler as *const VschedTaskImpl) };
            handler.task.set_state(AxTaskState::Ready);
            new_handler as *const ()
        }
    }
}
