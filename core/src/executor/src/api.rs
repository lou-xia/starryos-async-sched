use core::pin::Pin;

use alloc::{boxed::Box, sync::Arc};
use asynctask::{BaseScheduler, CurrentTask, Scheduler};
use axlog::info;
use kspin::SpinNoIrq;

use crate::{KERNEL_EXECUTOR, KERNEL_SCHEDULER, UTRAP_HANDLER, current::CurrentExecutor, executor::Executor, table::PID2PC};

pub fn init(utrap_handler: fn() -> Pin<Box<dyn Future<Output = isize> + 'static>>) {
    asynctask::init();
    UTRAP_HANDLER.init_once(utrap_handler);
    let mut scheduler = Scheduler::new();
    scheduler.init();
    KERNEL_SCHEDULER.init_once(Arc::new(SpinNoIrq::new(scheduler)));
    let kexecutor = Arc::new(Executor::new_init());
    KERNEL_EXECUTOR.init_once(kexecutor.clone());
    unsafe { CurrentExecutor::init_current(kexecutor) };
    #[cfg(feature = "irq")]
    asynctask::api::init();
    info!("  use {} scheduler.", Scheduler::scheduler_name());
}

pub fn init_secondary() {
    assert!(KERNEL_EXECUTOR.is_inited());
    asynctask::init();
    let kexecutor = KERNEL_EXECUTOR.clone();
    unsafe { CurrentExecutor::init_current(kexecutor) };
}

pub fn current_task_may_uninit() -> Option<CurrentTask> {
    CurrentTask::try_get()
}

pub fn current_task() -> CurrentTask {
    CurrentTask::get()
}

pub async fn current_executor() -> Arc<Executor> {
    let current_task = current_task();
    let current_process = Arc::clone(
        PID2PC
            .lock()
            // .await
            .get(&current_task.get_process_id())
            .unwrap(),
    );
    current_process
}