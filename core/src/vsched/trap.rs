use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::task::Poll;

use axtask::TaskState as AxTaskState;

use crate::config;

use super::task::{CoroutinePoll, VschedTaskImpl};
use super::trapframe::UserTrapFrame;
use super::{register_task, HIGHEST_PRIORITY};

type TrapDispatcher = fn(trapped_task: *const VschedTaskImpl);

// Last user task being serviced by trap handler (for page fault fallback)
static LAST_TRAPPED_USER_TASK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

pub fn set_last_trapped_user_task(task: *const ()) {
    LAST_TRAPPED_USER_TASK.store(task as usize, Ordering::Release);
}

pub fn get_last_trapped_user_task() -> *const VschedTaskImpl {
    LAST_TRAPPED_USER_TASK.load(Ordering::Acquire) as *const VschedTaskImpl
}

static TRAP_DISPATCHER: AtomicUsize = AtomicUsize::new(0);

pub fn register_trap_dispatcher(dispatcher: TrapDispatcher) {
    TRAP_DISPATCHER.store(dispatcher as usize, Ordering::Release);
}

// ---- TrapInfo implementation ----

pub struct VschedTrapInfoImpl {
    scause: usize,
    stval: usize,
    sepc: usize,
    trapped_task: usize,
}

impl libvsched2::TrapInfo for VschedTrapInfoImpl {
    fn from_task(task: *const ()) -> *const Self {
        let vti = unsafe { &*(task as *const VschedTaskImpl) };
        let tf_ptr = vti.trap_frame.load(Ordering::Acquire);
        let (scause, stval, sepc) = if tf_ptr != 0 {
            let tf = unsafe { &*(tf_ptr as *const UserTrapFrame) };
            (tf.scause, tf.stval, tf.sepc)
        } else {
            (0, 0, 0)
        };
        Box::into_raw(Box::new(Self {
            scause,
            stval,
            sepc,
            trapped_task: task as usize,
        }))
    }

    fn handle(&self, task: Option<*const ()>) {
        let dispatcher = TRAP_DISPATCHER.load(Ordering::Acquire);
        if dispatcher == 0 {
            return;
        }
        // Save the last USER (non-coroutine) trapped task for page fault fallback.
        // Nested traps (handler faults) need the original user task's AddrSpace.
        let trapped = self.trapped_task as *const VschedTaskImpl;
        if !trapped.is_null() {
            let vti = unsafe { &*trapped };
            if !vti.is_coroutine.load(Ordering::Acquire) {
                crate::vsched::trap::set_last_trapped_user_task(trapped as *const ());
            }
        }
        let dispatcher: TrapDispatcher = unsafe { core::mem::transmute(dispatcher) };
        if !trapped.is_null() {
            dispatcher(trapped);
        }
    }

    fn dealloc(&self) {
        let ptr = self as *const Self as *mut Self;
        unsafe { drop(Box::from_raw(ptr)) };
    }

    fn new_handler(queue: *const ()) -> *const () {
        let handler_fn = unsafe {
            libvsched2::VDSO_VTABLE
                .trap_handler
                .expect("trap_handler not in vtable")
        };
        let task_ref = axtask::new_raw(
            || {},
            alloc::string::String::from("trap_handler"),
            config::KERNEL_STACK_SIZE,
        );
        task_ref.set_state(AxTaskState::Blocked);
        let coro = Arc::new(TrapHandlerCoroutine {
            handler_fn: AtomicUsize::new(handler_fn as usize),
            queue: AtomicUsize::new(queue as usize),
        });
        let ptr = register_task(task_ref, HIGHEST_PRIORITY, 0, Some(coro));
        ptr as *const ()
    }
}

// ---- Handler coroutine ----

struct TrapHandlerCoroutine {
    handler_fn: AtomicUsize,
    queue: AtomicUsize,
}

unsafe impl Send for TrapHandlerCoroutine {}
unsafe impl Sync for TrapHandlerCoroutine {}

impl CoroutinePoll for TrapHandlerCoroutine {
    fn poll(&self) -> Poll<isize> {
        let handler = self.handler_fn.load(Ordering::Acquire);
        let queue = self.queue.load(Ordering::Acquire);
        let handler: fn(*const ()) = unsafe { core::mem::transmute(handler) };
        handler(queue as *const ());
        unsafe { core::ptr::write_volatile(0xffffffc010000000 as *mut u8, b'P'); }
        Poll::Pending
    }
}
