//! VschedTaskImpl — vsched2 Task 接口的 StarryOS 实现。

use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use core::task::Poll;

use axlog::ax_println;
use axmm;
use axtask::{AxTaskRef, TaskState as AxTaskState};
use memory_addr;

use super::trapframe::{UserTrapFrame, UserTrapFrameKind};
use super::{from_vsched_state, to_vsched_state};

/// 协程轮询接口。实现该 trait 的任务被 vsched2 作为协程调度。
pub trait CoroutinePoll: Send + Sync {
    fn poll(&self) -> Poll<isize>;
}

/// 封装 AxTaskRef 以适配 vsched2 Task trait。
pub struct VschedTaskImpl {
    pub task: AxTaskRef,
    pub priority: AtomicIsize,
    pub pid: AtomicUsize,
    pub is_coroutine: AtomicBool,
    pub return_value: AtomicIsize,
    pub thread_stack_base: AtomicUsize,
    /// 线程栈的 Stack 实现对象指针（`*mut VschedStackImpl`），由 `thread_stack()` 返回
    pub thread_stack_ptr: AtomicUsize,
    pub coroutine: Option<Arc<dyn CoroutinePoll>>,
    /// 用户态 vDSO 基址，`mm.rs` 加载进程时设置，`into_user` 用作 sepc 计算。
    pub user_vdso_base: AtomicUsize,
    /// 已保存的寄存器上下文指针（`*mut UserTrapFrame`），由 trap/yield 入口填入。
    pub trap_frame: AtomicUsize,
    /// 用户态页表根物理地址（仅用户任务设置），`into_user_context` 时切换到该页表。
    pub user_page_table_root: AtomicUsize,
    /// 用户地址空间，用于在 sret 前将最新内核映射复制到用户页表。
    pub user_aspace: AtomicUsize,
}

impl VschedTaskImpl {
    pub fn new(
        task: AxTaskRef,
        priority: isize,
        pid: usize,
        coroutine: Option<Arc<dyn CoroutinePoll>>,
    ) -> Self {
        Self {
            task,
            priority: AtomicIsize::new(priority),
            pid: AtomicUsize::new(pid),
            is_coroutine: AtomicBool::new(coroutine.is_some()),
            return_value: AtomicIsize::new(0),
            thread_stack_base: AtomicUsize::new(0),
            thread_stack_ptr: AtomicUsize::new(0),
            coroutine,
            trap_frame: AtomicUsize::new(0),
            user_page_table_root: AtomicUsize::new(0),
            user_aspace: AtomicUsize::new(0),
            user_vdso_base: AtomicUsize::new(0),
        }
    }

    pub fn inner(&self) -> &AxTaskRef {
        &self.task
    }
}

impl libvsched2::Task for VschedTaskImpl {
    fn state(&self) -> libvsched2::TaskState {
        to_vsched_state(self.task.state())
    }

    fn set_state(&self, state: libvsched2::TaskState) -> libvsched2::TaskState {
        let old = to_vsched_state(self.task.state());
        self.task.set_state(from_vsched_state(state));
        old
    }

    fn priority(&self) -> isize {
        self.priority.load(Ordering::Acquire)
    }

    fn is_coroutine(&self) -> bool {
        self.is_coroutine.load(Ordering::Acquire)
    }

    fn is_kernel(&self) -> bool {
        self.pid.load(Ordering::Acquire) == 0
    }

    fn pid(&self) -> usize {
        self.pid.load(Ordering::Acquire)
    }

    fn set_pid(&self, pid: usize) {
        self.pid.store(pid, Ordering::Release);
    }

    /// 保存当前 callee-saved 上下文，进入 raw_thread_entry。
    /// 下次被调度时从此处返回。
    fn resched(&self) {
        unsafe extern "C" {
            fn vsched_yield_trampoline();
        }
        unsafe { vsched_yield_trampoline() };
    }

    /// 从 `trap_frame` 恢复寄存器上下文。不返回——直接跳转到保存的指令。
    fn restore_context(&self) {
        let tf_ptr = self.trap_frame.load(Ordering::Acquire);
        assert_ne!(
            tf_ptr, 0,
            "restore_context: trap_frame is null — thread tasks must have an initial trap frame set before first scheduling"
        );
        let tf = unsafe { &*(tf_ptr as *const UserTrapFrame) };
        // 用户任务需要 sret 切至用户态 + 切换用户页表；
        // 内核任务用 jr（不改变特权级）。
        let user_root = self.user_page_table_root.load(Ordering::Acquire);
        if user_root != 0 {
            axlog::ax_println!("[restore] pid={} user_root={:#x} sepc={:#x} sp={:#x}",
                self.task.id_name(), user_root, tf.sepc, tf.regs.sp);
            let satp_val = (8usize << 60) | (user_root >> 12);
            unsafe { tf.restore_and_sret_user(satp_val) };
        } else {
            unsafe { tf.restore_and_jump() };
        }
    }

    fn poll(&self) -> Poll<isize> {
        match self.coroutine.as_ref() {
            Some(coro) => {
                let polled = coro.poll();
                if let Poll::Ready(value) = polled {
                    self.return_value.store(value, Ordering::Release);
                }
                polled
            }
            None => {
                panic!("Cannot poll a thread task: {}", self.task.id_name());
            }
        }
    }

    fn thread_stack(&self) -> *mut () {
        self.thread_stack_ptr.load(Ordering::Acquire) as *mut ()
    }

    fn set_return_value(&self, value: isize) {
        self.return_value.store(value, Ordering::Release);
    }

    fn dealloc(&self) {
        // 用户任务由 ProcessData Arc 管理生命周期，内核任务暂不回收
    }
}

/// 从 vsched2 返回的裸指针还原 AxTaskRef。
pub fn task_from_raw(task: *const ()) -> Option<AxTaskRef> {
    if task.is_null() {
        return None;
    }
    let vti = unsafe { &*(task as *const VschedTaskImpl) };
    Some(vti.task.clone())
}

/// 创建 VschedTaskImpl 并返回裸指针。
/// 调用前需先通过 `set_process_vdso_base` 设置当前进程的 vDSO 基址。
pub fn register_task(
    task: AxTaskRef,
    priority: isize,
    pid: usize,
    coroutine: Option<Arc<dyn CoroutinePoll>>,
) -> *const VschedTaskImpl {
    let vdso_base = super::context::get_process_vdso_base();
    let mut vti = Box::new(VschedTaskImpl::new(task, priority, pid, coroutine));
    vti.user_vdso_base.store(vdso_base, Ordering::Release);
    Box::into_raw(vti)
}
