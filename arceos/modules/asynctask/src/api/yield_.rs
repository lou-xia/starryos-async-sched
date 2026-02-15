use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use kernel_guard::{BaseGuard, NoPreemptIrqSave};

#[derive(Debug)]
pub struct YieldFuture {
    _has_polled: bool,
    _irq_state: <NoPreemptIrqSave as BaseGuard>::State,
}

impl YieldFuture {
    pub fn new() -> Self {
        // 这里获取中断状态，并且关中断
        #[cfg(feature = "thread")]
        let _irq_state = Default::default();
        #[cfg(not(feature = "thread"))]
        let _irq_state = NoPreemptIrqSave::acquire();
        Self {
            _has_polled: false,
            _irq_state,
        }
    }
}

impl Future for YieldFuture {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        #[cfg(feature = "thread")]
        return Poll::Ready(());
        #[cfg(not(feature = "thread"))]
        {
            let this = self.get_mut();
            if this._has_polled {
                // 恢复原来的中断状态
                NoPreemptIrqSave::release(this._irq_state);
                Poll::Ready(())
            } else {
                this._has_polled = true;
                Poll::Pending
            }
        }
    }
}
