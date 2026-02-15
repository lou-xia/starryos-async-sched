use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use kernel_guard::{BaseGuard, NoPreemptIrqSave};

#[derive(Debug)]
pub struct ExitFuture {
    _irq_state: <NoPreemptIrqSave as BaseGuard>::State,
}

impl ExitFuture {
    pub fn new() -> Self {
        // 这里获取中断状态，并且关中断
        #[cfg(feature = "thread")]
        let _irq_state = Default::default();
        #[cfg(not(feature = "thread"))]
        let _irq_state = NoPreemptIrqSave::acquire();
        Self { _irq_state }
    }
}

impl Future for ExitFuture {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        #[cfg(feature = "thread")]
        return Poll::Ready(());
        #[cfg(not(feature = "thread"))]
        {
            self.get_mut()._irq_state = NoPreemptIrqSave::acquire();
            Poll::Pending
        }
    }
}
