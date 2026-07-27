use alloc::{boxed::Box, sync::Arc};
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    task::Poll,
};

use axtask::TaskState as AxTaskState;
use kernel_guard::{BaseGuard, IrqSave};
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
    /// Hardware interrupt controller state is per-hart.  The first handling
    /// of a deferred external IRQ must therefore stay on its source CPU.
    origin_cpu: usize,
}

impl libvsched2::TrapInfo for VschedTrapInfoImpl {
    fn from_task(task: *const ()) -> *const Self {
        let vti = unsafe { &*(task as *const VschedTaskImpl) };
        let tf_ptr = vti.trap_frame.load(Ordering::Acquire);
        assert_ne!(tf_ptr, 0, "TrapInfo::from_task: task has no trap frame");
        let frame = unsafe { *(tf_ptr as *const UserTrapFrame) };
        Box::into_raw(Box::new(Self {
            frame,
            origin_cpu: <super::smp::VschedSmpImpl as SMP>::cpu_id(),
        }))
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
        let is_external_irq = self.frame.scause == 0x8000000000000009;
        if is_external_irq {
            assert_eq!(
                self.origin_cpu,
                <super::smp::VschedSmpImpl as SMP>::cpu_id(),
                "deferred external IRQ migrated before PLIC claim/complete",
            );
        }

        // TrapHandler 在操作 vsched2 的 trap/ready 队列时保持关中断；只在
        // StarryOS 实际处理 syscall/IRQ 的区间打开本地中断。这样阻塞型
        // syscall 可以被 IRQ 打断，同时不会在持有 trap_wait_queue 锁时
        // 重入同一队列。
        IrqSave::release(1 << 1);
        dispatcher(trapped, &self.frame);
        let irq_state = IrqSave::acquire();
        assert_eq!(
            irq_state,
            1 << 1,
            "trap dispatcher returned with IRQs disabled"
        );
        if is_external_irq {
            // The dispatcher has completed PLIC claim/handler/complete.  It is
            // now safe to accept another external interrupt on this hart.
            unsafe {
                core::arch::asm!("csrs sie, {seie}", seie = in(reg) 1usize << 9);
            }
        }
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

        // IrqCorotineWrapper 恢复的是任务执行时的中断状态；vsched2 的
        // handler 队列管理本身仍要求关中断。handler 正常通过 resched
        // 非局部离开，不能依赖 guard 的 Drop，因此显式保存/恢复状态。
        let irq_state = IrqSave::acquire();
        handler(queue as *const ());
        IrqSave::release(irq_state);
        Poll::Pending
    }
}

/// 交替切换当前 vsched2 任务的 is_coroutine 状态。
///
/// 第一次调用发生在 `block_on` 已经原子提交 Blocking 且发布 Parked 之后：
/// 将任务标记为线程。随后统一由主动让权入口保存 continuation 并把当前
/// 协程栈交给该任务，避免 block_on 和普通线程各自实现一套栈交接协议。
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
        // transition_block_on_task() has already committed Blocking.  vsched2
        // will change it to Blocked only after the common yield entry has
        // detached the continuation stack from this CPU.
        vti.is_coroutine.store(false, Ordering::Release);
        axlog::ax_println!("[block_on] coroutine -> thread task={:#x}", ptr as usize);
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
