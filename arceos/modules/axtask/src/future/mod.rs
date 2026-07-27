//! Future support.

use alloc::{
    sync::Arc,
    task::Wake,
};
use core::{
    fmt,
    future::poll_fn,
    pin::pin,
    sync::atomic::{AtomicU8, Ordering},
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
    /// Cross-CPU handoff between the polling task and its Waker.
    ///
    /// A Waker may only touch the vsched2 task state after this value reaches
    /// PARKED.  Before that point it leaves a NOTIFIED token, which makes the
    /// polling CPU cancel Blocking and repoll instead of losing the wake.
    vsched_wait: AtomicU8,
    /// The vsched2 task that owns the suspended continuation.  This is not
    /// necessarily the axtask task stored in `task`: a reusable TrapHandler
    /// executes with the trapped user task as its temporary axtask identity.
    vsched_task: *const (),
    vsched_generation: usize,
}

const WAIT_IDLE: u8 = 0;
const WAIT_PARKING: u8 = 1;
const WAIT_PARKED: u8 = 2;
const WAIT_NOTIFIED: u8 = 3;

unsafe impl Send for AxWaker {}
unsafe impl Sync for AxWaker {}

impl AxWaker {
    fn new(task: &AxTaskRef) -> Arc<Self> {
        let (vsched_task, vsched_generation) = crate::api::block_on_current_task()
            .unwrap_or((core::ptr::null(), 0));
        Arc::new(AxWaker {
            task: Arc::downgrade(task),
            woke: SpinNoIrq::new(false),
            vsched_wait: AtomicU8::new(WAIT_IDLE),
            vsched_task,
            vsched_generation,
        })
    }

    /// Starts the wait-side handshake.  A notification that arrived while the
    /// Future was being polled is consumed here and forces an immediate repoll.
    fn begin_park(&self) -> bool {
        loop {
            match self.vsched_wait.load(Ordering::Acquire) {
                WAIT_IDLE => {
                    if self
                        .vsched_wait
                        .compare_exchange_weak(
                            WAIT_IDLE,
                            WAIT_PARKING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return true;
                    }
                }
                WAIT_NOTIFIED => {
                    if self
                        .vsched_wait
                        .compare_exchange_weak(
                            WAIT_NOTIFIED,
                            WAIT_IDLE,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return false;
                    }
                }
                state => panic!("block_on: invalid begin wait state {state}"),
            }
        }
    }

    /// Publishes that task state is already Blocking.  Once this succeeds, a
    /// remote Waker may change Blocking to Ready; before it succeeds, a remote
    /// notification makes this return false so Blocking can be cancelled.
    fn commit_park(&self) -> bool {
        match self.vsched_wait.compare_exchange(
            WAIT_PARKING,
            WAIT_PARKED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(WAIT_NOTIFIED) => {
                self.vsched_wait
                    .compare_exchange(
                        WAIT_NOTIFIED,
                        WAIT_IDLE,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .expect("block_on: notification changed while cancelling park");
                false
            }
            Err(state) => panic!("block_on: invalid commit wait state {state}"),
        }
    }

    fn abort_park(&self) {
        self.vsched_wait
            .compare_exchange(
                WAIT_PARKING,
                WAIT_IDLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("block_on: invalid aborted wait state");
    }

    fn finish_park(&self) {
        self.vsched_wait
            .compare_exchange(
                WAIT_NOTIFIED,
                WAIT_IDLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("block_on: resumed without a committed notification");
    }

    /// Records a wake and returns true only when the task has fully published
    /// PARKED.  Exactly that transition owns the call into the vsched2 wake
    /// hook; all earlier transitions are consumed by the polling CPU.
    fn notify(&self) -> bool {
        loop {
            let state = self.vsched_wait.load(Ordering::Acquire);
            let (new_state, wake_task) = match state {
                WAIT_IDLE | WAIT_PARKING => (WAIT_NOTIFIED, false),
                WAIT_PARKED => (WAIT_NOTIFIED, true),
                WAIT_NOTIFIED => return false,
                _ => unreachable!(),
            };
            if self
                .vsched_wait
                .compare_exchange_weak(
                    state,
                    new_state,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return wake_task;
            }
        }
    }
}

impl Wake for AxWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        *self.woke.lock() = true;
        if crate::api::vsched2_active() && !self.vsched_task.is_null() {
            if self.notify() {
                // PARKED guarantees that the polling CPU has already committed
                // task state to Blocking.  The wake callback can therefore use
                // the existing Blocking/Blocked -> Ready protocol on any CPU.
                let _ = crate::api::wake_block_on_task(
                    self.vsched_task,
                    self.vsched_generation,
                );
            }
        } else if let Some(task) = self.task.upgrade() {
            if !crate::api::vsched2_active() {
                select_run_queue::<NoPreemptIrqSave>(&task).unblock_task(task, false);
            }
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
        let curr = current();
        let task = curr.clone();
        let axwaker = AxWaker::new(&task);
        let waker = Waker::from(axwaker.clone());
        let mut cx = Context::from_waker(&waker);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Pending => {
                    // raw_thread_entry and take_current_stack require local
                    // interrupts to be disabled.  Cross-CPU correctness comes
                    // from AxWaker's atomic Parking/Parked/Notified handshake,
                    // not from this local interrupt guard.
                    let guard = NoPreemptIrqSave::new();
                    if !axwaker.begin_park() {
                        drop(guard);
                        continue;
                    }

                    let state_committed = crate::api::transition_block_on_task(true)
                        .expect("block_on: vsched2 state hook is not registered");
                    if !state_committed {
                        axwaker.abort_park();
                        drop(guard);
                        continue;
                    }
                    if !axwaker.commit_park() {
                        assert!(
                            crate::api::transition_block_on_task(false)
                                .expect("block_on: vsched2 state hook is not registered"),
                            "block_on: failed to cancel Blocking state"
                        );
                        drop(guard);
                        continue;
                    }

                    let toggle = crate::api::BLOCK_ON_TOGGLE
                        .load(core::sync::atomic::Ordering::Acquire);
                    let promoted = if toggle != 0 {
                        let f: fn(bool) -> bool = unsafe { core::mem::transmute(toggle) };
                        f(true)
                    } else {
                        false
                    };
                    crate::yield_now();
                    if promoted {
                        let f: fn(bool) -> bool = unsafe { core::mem::transmute(toggle) };
                        let restored = f(false);
                        debug_assert!(restored, "block_on: failed to restore coroutine context");
                    }
                    axwaker.finish_park();
                    drop(guard);
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
        f.write_str("interrupted")
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
