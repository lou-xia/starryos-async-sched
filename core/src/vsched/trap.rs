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
        // Keep the originating user task available only while its dispatcher is
        // active, so a nested handler fault can resolve the correct AddrSpace.
        let cached_user = trapped.filter(|ptr| {
            let vti = unsafe { &**ptr };
            !vti.is_kernel()
        });
        if let Some(user) = cached_user {
            set_last_trapped_user_task(user as *const ());
        }
        let dispatcher: TrapDispatcher = unsafe { core::mem::transmute(dispatcher) };
        dispatcher(trapped, &self.frame);
        if let Some(user) = cached_user {
            clear_last_trapped_user_task(user);
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
        // 为 handler 预分配一个栈，供 toggle 到线程模式时使用
        let handler_stack = super::alloc_stack();
        set_handler_stack(handler_stack as usize);
        axlog::ax_println!("[new_handler] stack={:#x}", handler_stack as usize);
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

/// 记录 handler 的协程栈 VSI 指针，供 toggle 时设置 thread_stack_ptr 使用。
static HANDLER_STACK: [AtomicUsize; CPU_NUM] = [const { AtomicUsize::new(0) }; CPU_NUM];

pub fn set_handler_stack(stack: usize) {
    HANDLER_STACK[<super::smp::VschedSmpImpl as SMP>::cpu_id()].store(stack, Ordering::Release);
}

/// 交替切换 handler 的 is_coroutine 状态。
/// 第一次调用: coroutine → thread（yield 前）
/// 第二次调用: thread → coroutine（yield 恢复后）
pub fn toggle_handler() {
    let ptr = libvsched2::current_task_ptr() as *const super::VschedTaskImpl;
    if ptr.is_null() { return; }
    let vti = unsafe { &*ptr };
    if vti.pid.load(Ordering::Acquire) != 0 { return; }

    let is_coro = vti.is_coroutine.load(Ordering::Acquire);
    if is_coro {
        // vsched2 requires Blocking before the context-save path commits the
        // task to Blocked.  Setting Blocked here would bypass that protocol.
        vti.set_state(libvsched2::TaskState::Blocking);
        axlog::ax_println!("[toggle] coroutine → thread #{}", {
            static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
            N.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
        });
        let stack = HANDLER_STACK[<super::smp::VschedSmpImpl as SMP>::cpu_id()].load(Ordering::Acquire);
        vti.thread_stack_ptr.store(stack, Ordering::Release);
        vti.is_coroutine.store(false, Ordering::Release);
    } else {
        axlog::ax_println!("[toggle] thread → coroutine");
        vti.is_coroutine.store(true, Ordering::Release);
        vti.thread_stack_ptr.store(0, Ordering::Release);
    }
}
