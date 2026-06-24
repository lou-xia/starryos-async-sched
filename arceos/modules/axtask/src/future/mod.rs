//! Future support.

use alloc::{
    sync::Arc,
    task::Wake,
};
use core::{
    fmt,
    future::poll_fn,
    pin::pin,
    task::{Context, Poll, Waker},
};

use axerrno::AxError;
use kernel_guard::NoPreemptIrqSave;
use kspin::SpinNoIrq;

use crate::{AxTaskRef, WeakAxTaskRef, current, current_run_queue, select_run_queue};

mod poll;
pub use poll::*;

mod time;
pub use time::*;

struct AxWaker {
    task: WeakAxTaskRef,
    woke: SpinNoIrq<bool>,
}

impl AxWaker {
    fn new(task: &AxTaskRef) -> Arc<Self> {
        Arc::new(AxWaker {
            task: Arc::downgrade(task),
            woke: SpinNoIrq::new(false),
        })
    }
}

impl Wake for AxWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(task) = self.task.upgrade() {
            *self.woke.lock() = true;
            select_run_queue::<NoPreemptIrqSave>(&task).unblock_task(task, false);
        }
    }
}

/// Blocks the current task until the given future is resolved.
///
/// Note that this doesn't handle interruption and is not recommended for direct
/// use in most cases.
pub fn block_on<F: IntoFuture>(f: F) -> F::Output {
    let mut fut = pin!(f.into_future());

    if crate::api::vsched2_active() {
        // vsched2 active: poll + yield loop.
        // Each yield_now() goes through vsched2's yield trampoline → kschedule.
        // Use AxWaker so external wakes (child_exit_event, futex, etc.) set the
        // `woke` flag. The poll_fn (e.g. wait4's check_children) re-checks
        // state on each iteration. When the child exits, check_children finds
        // the zombie and returns Ready.
        let curr = current();
        let task = curr.clone();
        let axwaker = AxWaker::new(&task);
        let waker = Waker::from(axwaker.clone());
        let mut cx = Context::from_waker(&waker);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Pending => {
                    let woke = axwaker.woke.lock();
                    if !*woke {
                        drop(woke);
                        // Mark Ready so vsched2's push_prev_task re-queues us.
                        // Without this, Blocked tasks are invisible to push_prev_task
                        // and get permanently lost from the ready_queue.
                        task.set_state(crate::TaskState::Ready);
                        crate::yield_now();
                    } else {
                        drop(woke);
                    }
                }
                Poll::Ready(output) => return output,
            }
        }
    }

    // Original AxRunQueue path (when vsched2 is not active):
    let curr = current();
    let task = curr.clone();
    let axwaker = AxWaker::new(&task);
    let waker = Waker::from(axwaker.clone());
    let mut cx = Context::from_waker(&waker);

    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Pending => {
                let mut rq = current_run_queue::<NoPreemptIrqSave>();
                let woke = axwaker.woke.lock();
                if !*woke {
                    rq.blocked_resched(woke);
                } else {
                    drop(woke);
                    crate::yield_now();
                }
            }
            Poll::Ready(output) => break output,
        }
    }
}

/// Error returned by [`interruptible`].
#[derive(Debug, PartialEq, Eq)]
pub struct Interrupted;

impl fmt::Display for Interrupted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "interrupted")
    }
}

impl core::error::Error for Interrupted {}

impl From<Interrupted> for AxError {
    fn from(_: Interrupted) -> Self {
        AxError::Interrupted
    }
}

/// Makes a future interruptible.
pub async fn interruptible<F: IntoFuture>(f: F) -> Result<F::Output, Interrupted> {
    let mut f = pin!(f.into_future());
    let curr = current();
    poll_fn(|cx| {
        if curr.poll_interrupt(cx).is_ready() {
            return Poll::Ready(Err(Interrupted));
        }
        f.as_mut().poll(cx).map(Ok)
    })
    .await
}
