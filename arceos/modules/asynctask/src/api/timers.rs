use axhal::time::monotonic_time;
use core::task::Waker;
use lazyinit::LazyInit;
use kspin::SpinNoIrq;
use timer_list::{TimeValue, TimerEvent, TimerList};

// TODO: per-CPU
static TIMER_LIST: LazyInit<SpinNoIrq<TimerList<TaskWakeupEvent>>> = LazyInit::new();

struct TaskWakeupEvent(Waker);

impl TimerEvent for TaskWakeupEvent {
    fn callback(self, _now: TimeValue) {
        self.0.wake();
    }
}

pub fn set_alarm_wakeup(deadline: TimeValue, waker: Waker) {
    let task = waker.data() as *const crate::Task;
    unsafe { &*task }.set_state(crate::task::TaskState::Blocking);
    let mut timer_list = TIMER_LIST.lock();
    timer_list.set(deadline, TaskWakeupEvent(waker));
    drop(timer_list)
}

pub fn cancel_alarm(waker: &Waker) {
    TIMER_LIST.lock().cancel(|t| Waker::will_wake(&t.0, waker));
}

pub fn check_events() {
    loop {
        let now = monotonic_time();
        let event = TIMER_LIST.lock().expire_one(now);
        if let Some((_deadline, event)) = event {
            event.callback(now);
        } else {
            break;
        }
    }
}

pub fn init() {
    TIMER_LIST.init_once(SpinNoIrq::new(TimerList::new()));
}
