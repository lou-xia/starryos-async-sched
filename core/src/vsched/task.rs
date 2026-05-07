use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use core::task::Poll;

use axtask::{AxTaskRef, TaskState as AxTaskState};

use crate::config;

use super::{from_vsched_state, to_vsched_state};

pub trait CoroutinePoll: Send + Sync {
    fn poll(&self) -> Poll<usize>;
}

pub struct VschedTaskImpl {
    pub task: AxTaskRef,
    pub priority: AtomicIsize,
    pub pid: AtomicUsize,
    pub is_coroutine: AtomicBool,
    pub return_value: AtomicUsize,
    pub thread_stack_base: AtomicUsize,
    pub coroutine: Option<Arc<dyn CoroutinePoll>>,
}

impl VschedTaskImpl {
    pub fn new(
        task: AxTaskRef,
        priority: isize,
        pid: usize,
        coroutine: Option<Arc<dyn CoroutinePoll>>,
    ) -> Self {
        Self {
            task,
            priority: AtomicIsize::new(priority),
            pid: AtomicUsize::new(pid),
            is_coroutine: AtomicBool::new(coroutine.is_some()),
            return_value: AtomicUsize::new(0),
            thread_stack_base: AtomicUsize::new(0),
            coroutine,
        }
    }

    pub fn inner(&self) -> &AxTaskRef {
        &self.task
    }
}

impl libvsched2::Task for VschedTaskImpl {
    fn state(&self) -> libvsched2::TaskState {
        to_vsched_state(self.task.state())
    }

    fn set_state(&self, state: libvsched2::TaskState) -> libvsched2::TaskState {
        let old = to_vsched_state(self.task.state());
        self.task.set_state(from_vsched_state(state));
        old
    }

    fn priority(&self) -> isize {
        self.priority.load(Ordering::Acquire)
    }

    fn is_coroutine(&self) -> bool {
        self.is_coroutine.load(Ordering::Acquire)
    }

    fn pid(&self) -> usize {
        self.pid.load(Ordering::Acquire)
    }

    fn set_pid(&self, pid: usize) {
        self.pid.store(pid, Ordering::Release);
    }

    // TODO
    fn save_thread_context(&self) {
        self.task.set_state(AxTaskState::Ready);
    }

    // TODO
    fn save_trap_context(&self) {
        self.task.set_state(AxTaskState::Blocked);
    }

    // TODO
    fn restore_context(&self) {
        panic!(
            "VschedTaskImpl::restore_context: vsched2 context switching not yet integrated. \
             task={}",
            self.task.id_name()
        );
    }

    fn poll(&self) -> Poll<usize> {
        match self.coroutine.as_ref() {
            Some(coro) => {
                let polled = coro.poll();
                if let Poll::Ready(value) = polled {
                    self.return_value.store(value, Ordering::Release);
                }
                polled
            }
            None => {
                panic!("Cannot poll a thread task: {}", self.task.id_name());
            }
        }
    }

    fn thread_stack_base(&self) -> usize {
        self.thread_stack_base.load(Ordering::Acquire)
    }

    fn set_return_value(&self, value: usize) {
        self.return_value.store(value, Ordering::Release);
    }
}

pub fn task_from_raw(task: *const ()) -> Option<AxTaskRef> {
    if task.is_null() {
        return None;
    }
    let vti = unsafe { &*(task as *const VschedTaskImpl) };
    Some(vti.task.clone())
}

pub fn register_task(
    task: AxTaskRef,
    priority: isize,
    pid: usize,
    coroutine: Option<Arc<dyn CoroutinePoll>>,
) -> *const VschedTaskImpl {
    let vti = Box::new(VschedTaskImpl::new(task, priority, pid, coroutine));
    Box::into_raw(vti)
}
