// use executor::current_task_may_uninit;
use asynctask::{TrapFrame, TrapStatus};

#[cfg(feature = "irq")]
#[doc(cfg(feature = "irq"))]
/// Handles periodic timer ticks for the task manager.
///
/// For example, advance scheduler states, checks timed events, etc.
pub fn on_timer_tick() {
    use asynctask::{BaseScheduler, CurrentTask};
    asynctask::api::check_events();
    // warn!("on_timer_tick");
    if let Some(curr) = CurrentTask::try_get() {
        if curr.get_scheduler().lock().task_tick(curr.as_task_ref()) {
            #[cfg(feature = "preempt")]
            curr.set_preempt_pending(true);
        }
    }
}

pub fn handle_irq(_irq_num: usize, tf: &mut TrapFrame) {
    #[cfg(feature = "irq")]
    {
        let guard = kernel_guard::NoPreempt::new();
        axhal::irq::handle(_irq_num);
        drop(guard); // rescheduling may occur when preemption is re-enabled.
        tf.trap_status = TrapStatus::Done;

        #[cfg(feature = "preempt")]
        crate::current_check_preempt_pending(tf);
    }
}

pub async fn handle_user_irq(_irq_num: usize, tf: &mut TrapFrame) {
    #[cfg(feature = "irq")]
    {
        let guard = kernel_guard::NoPreempt::new();
        axhal::irq::handle(_irq_num);
        drop(guard); // rescheduling may occur when preemption is re-enabled.

        tf.trap_status = TrapStatus::Done;
        #[cfg(feature = "preempt")]
        crate::current_check_user_preempt_pending(tf).await;
    }
}
