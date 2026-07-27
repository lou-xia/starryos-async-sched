use alloc::{boxed::Box, sync::Arc};
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    task::Poll,
};

use axtask::TaskState as AxTaskState;
use libvsched2::{self, SMP, Task};

use super::{
    HIGHEST_PRIORITY, register_task,
    task::{CoroutinePoll, VschedTaskImpl},
    trapframe::UserTrapFrame,
};
use crate::config;

type TrapDispatcher = fn(Option<*const VschedTaskImpl>, &UserTrapFrame);

const CPU_NUM: usize = axconfig::plat::CPU_NUM;

// Last user task being serviced by trap handler (for page fault fallback)
static LAST_TRAPPED_USER_TASK: [core::sync::atomic::AtomicUsize; CPU_NUM] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; CPU_NUM];

pub fn set_last_trapped_user_task(task: *const ()) {
    LAST_TRAPPED_USER_TASK[<super::smp::VschedSmpImpl as SMP>::cpu_id()]
        .store(task as usize, Ordering::Release);
}

pub fn get_last_trapped_user_task() -> *const VschedTaskImpl {
    let current = libvsched2::current_task_ptr() as *const VschedTaskImpl;
    if !current.is_null() {
        let owner = unsafe { &*current }.trap_owner.load(Ordering::Acquire);
        if owner != 0 {
            return owner as *const VschedTaskImpl;
        }
    }
    LAST_TRAPPED_USER_TASK[<super::smp::VschedSmpImpl as SMP>::cpu_id()]
        .load(Ordering::Acquire) as *const VschedTaskImpl
}

/// Clears a cached user task only if it is still the cache owner.
fn clear_last_trapped_user_task(task: *const VschedTaskImpl) {
    let slot = &LAST_TRAPPED_USER_TASK[<super::smp::VschedSmpImpl as SMP>::cpu_id()];
    let _ = slot.compare_exchange(
        task as usize,
        0,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}

static TRAP_DISPATCHER: AtomicUsize = AtomicUsize::new(0);

pub fn register_trap_dispatcher(dispatcher: TrapDispatcher) {
    TRAP_DISPATCHER.store(dispatcher as usize, Ordering::Release);
}

fn effective_user_owner(mut task: *const VschedTaskImpl) -> Option<*const VschedTaskImpl> {
    // Nested kernel traps form a short owner chain (handler -> user task).
    // Bound the walk so corrupted owner metadata cannot loop forever.
    for _ in 0..8 {
        if task.is_null() {
            return None;
        }
        let vti = unsafe { &*task };
        if !vti.is_kernel() {
            return Some(task);
        }
        task = vti.trap_owner.load(Ordering::Acquire) as *const VschedTaskImpl;
    }
    panic!("trap owner chain is cyclic or too deep");
}

// ---- TrapInfo implementation ----

pub struct VschedTrapInfoImpl {
    /// TrapInfo owns the immutable event snapshot.  The task's stable frame is
    /// the eventual resume target and may be updated independently.
    frame: UserTrapFrame,
}

impl libvsched2::TrapInfo for VschedTrapInfoImpl {
    fn from_task(task: *const ()) -> *const Self {
        let vti = unsafe { &*(task as *const VschedTaskImpl) };
        let tf_ptr = vti.trap_frame.load(Ordering::Acquire);
        assert_ne!(tf_ptr, 0, "TrapInfo::from_task: task has no trap frame");
        let frame = unsafe { *(tf_ptr as *const UserTrapFrame) };
        Box::into_raw(Box::new(Self { frame }))
    }

    fn handle(&self, task: Option<*const ()>) {
        let dispatcher = TRAP_DISPATCHER.load(Ordering::Acquire);
        if dispatcher == 0 {
            return;
        }
        // `task` is authoritative: vsched2 passes None for external interrupts.
        let trapped = task.map(|ptr| ptr as *const VschedTaskImpl);
        let owner = trapped.and_then(effective_user_owner);
        let handler = libvsched2::current_task_ptr() as *const VschedTaskImpl;
        if let Some(owner) = owner {
            assert!(!handler.is_null(), "TrapInfo::handle: no current handler");
            unsafe { &*handler }.bind_execution_task(owner);
        }
        let dispatcher: TrapDispatcher = unsafe { core::mem::transmute(dispatcher) };
        dispatcher(trapped, &self.frame);
        if owner.is_some() {
            unsafe { &*handler }.unbind_execution_task();
        }
    }

    fn dealloc(&self) {
        let ptr = self as *const Self as *mut Self;
        unsafe { drop(Box::from_raw(ptr)) };
    }

// axlog::ax_println!("[new_handler] START queue={:#x}", queue as usize);
    fn new_handler(queue: *const ()) -> *const () {
        let handler_fn = unsafe {
            libvsched2::VDSO_VTABLE
                .trap_handler
                .expect("trap_handler not in vtable")
// axlog::ax_println!("[new_handler] got handler_fn, creating task");
        };
        let task_ref = axtask::new_raw(
            || {},
            alloc::string::String::from("trap_handler"),
            config::KERNEL_STACK_SIZE,
// axlog::ax_println!("[new_handler] axtask::new_raw done");
        );
        task_ref.set_state(AxTaskState::Blocked);
        let coro = Arc::new(TrapHandlerCoroutine {
            handler_fn: AtomicUsize::new(handler_fn as usize),
            queue: AtomicUsize::new(queue as usize),
// axlog::ax_println!("[new_handler] about to register_task");
        });
// axlog::ax_println!("[new_handler] DONE ptr={:#x}", ptr as usize);
        let ptr = register_task(task_ref, HIGHEST_PRIORITY, 0, true, Some(coro), 0);
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
        let handler_fn = self.handler_fn.load(Ordering::Acquire);
        let queue = self.queue.load(Ordering::Acquire);
        let handler: fn(*const ()) = unsafe { core::mem::transmute(handler_fn) };
        handler(queue as *const ());
        Poll::Pending
    }
}

/// 交替切换当前 vsched2 任务的 is_coroutine 状态。
///
/// 第一次调用发生在 `block_on` 已经原子提交 Blocking 且发布 Parked 之后：取走当前 CPU 正在使用的真实协程栈，并交给该任务作为线程栈。这里不能使用一个 per-CPU 的 handler 栈槽，因为 block_on 可以由多个独立任务调用。
///
/// 第二次调用发生在原线程栈恢复后：任务恢复协程态，使下一次调度按根 Future poll 路径处理。
pub fn toggle_handler(promote: bool) -> bool {
    let ptr = libvsched2::current_task_ptr() as *const super::VschedTaskImpl;
    if ptr.is_null() {
        return false;
    }
    let vti = unsafe { &*ptr };

    let is_coro = vti.is_coroutine.load(Ordering::Acquire);
    if promote {
        if !is_coro {
            // Ordinary vsched2 threads already own a persistent stack and do
            // not need a coroutine conversion around block_on.
            return false;
        }
        // take_current_stack() must be called while interrupts are disabled;
        // block_on holds NoPreemptIrqSave around this callback.
        let stack = libvsched2::take_current_stack();
        assert!(!stack.is_null(), "toggle_handler: current stack is null");
        vti.thread_stack_ptr.store(stack as usize, Ordering::Release);
        // transition_block_on_task() has already committed Blocking.  vsched2
        // will change it to Blocked only after the continuation is safe.
        vti.is_coroutine.store(false, Ordering::Release);
        axlog::ax_println!("[block_on] coroutine -> thread task={:#x} stack={:#x}",
            ptr as usize, stack as usize);
        true
    } else {
        if is_coro {
            return false;
        }
        vti.is_coroutine.store(true, Ordering::Release);
        vti.thread_stack_ptr.store(0, Ordering::Release);
        axlog::ax_println!("[block_on] thread -> coroutine task={:#x}", ptr as usize);
        true
    }
}
