#![no_std]
#![feature(fn_align)]
#![feature(stmt_expr_attributes)]
#![feature(doc_cfg)]

extern crate alloc;
#[macro_use]
extern crate log;

mod arch;
mod executor_api;
mod fs_api;
mod init_api;
mod task_api;
mod trap_api;

use alloc::sync::Arc;
pub use arch::init_interrupt;
use asynctask::{BaseScheduler, CurrentTask, TaskState, TrapStatus};
use core::task::{Context, Poll};
pub use fs_api::fs_init;
pub use init_api::*;
pub use asynctask::TrapFrame;

pub use executor_api::*;
use riscv::{asm, register::scause::{self, Trap}};
pub use task_api::*;
pub use trap_api::*;

/// 进入 Trampoline 的方式：
///   1. 初始化后函数调用：没有 Trap，但存在就绪任务
///   2. 内核发生 Trap：存在任务被打断（CurrentTask 不为空），或者没有任务被打断（CurrentTask 为空）
///   3. 用户态发生 Trap：任务被打断，CurrentTask 不为空
///
/// 内核发生 Trap 时，将 TrapFrame 保存在内核栈上
/// 在用户态发生 Trap 时，将 TrapFrame 直接保存在任务控制块中，而不是在内核栈上
///
/// 只有通过 trap 进入这个入口时，是处于关中断的状态，剩下的任务切换是没有关中断
#[unsafe(no_mangle)]
pub fn trampoline(tf: &mut TrapFrame, has_trap: bool, from_user: bool) {
    loop {
        if !from_user && has_trap {
            // 在内核中发生了 Trap，只处理中断，目前还不支持抢占，因此是否有任务被打断是不做处理的
            let scause = scause::read();
            match scause.cause() {
                Trap::Interrupt(_interrupt) => handle_irq(tf.get_scause_code(), tf),
                Trap::Exception(e) => {
                    panic!(
                        "Unsupported kernel trap {:?} @ {:#x}:\n{:#x?}",
                        e, tf.sepc, tf
                    )
                }
            }
            return;
        } else {
            // 用户态发生了 Trap 或者需要调度
            if let Some(curr) = CurrentTask::try_get().or_else(|| {
                if let Some(task) = CurrentExecutor::get().pick_next_task() {
                    unsafe {
                        CurrentTask::init_current(task);
                    }
                    Some(CurrentTask::get())
                } else {
                    None
                }
            }) {
                run_task(curr);
            } else {
                axhal::asm::enable_irqs();
                // 没有就绪任务，等待中断
                #[cfg(feature = "irq")]
                axhal::asm::wait_for_irqs();
            }
        }
    }
}

const IS_ASYNC: usize = 0x5f5f5f5f;

pub fn run_task(curr: CurrentTask) {
    let waker = curr.waker();
    let cx = &mut Context::from_waker(&waker);
    let page_table_token = curr.get_page_table_token();
    if page_table_token != 0 {
        let page_table_token_pa = page_table_token.into();
        unsafe {
            // axhal::arch::write_page_table_root0(page_table_token.into());
            if page_table_token_pa != axhal::asm::read_user_page_table() {
                axhal::asm::write_user_page_table(page_table_token_pa);
                asm::sfence_vma_all();
            }
            #[cfg(target_arch = "riscv64")]
            riscv::register::sstatus::set_sum();
        };
    }
    #[cfg(any(feature = "thread", feature = "preempt"))]
    restore_from_stack_ctx(curr.as_task_ref());
    // warn!("run task {} count {}", task.id_name(), Arc::strong_count(task));
    let res = curr.get_fut().as_mut().poll(cx);
    match res {
        Poll::Ready(exit_code) => {
            debug!("task exit: {}, exit_code={}", curr.id_name(), exit_code);
            curr.set_state(TaskState::Exited);
            curr.set_exit_code(exit_code);
            curr.notify_waker_for_exit();
            if curr.is_init() {
                assert!(
                    Arc::strong_count(curr.as_task_ref()) == 1,
                    "count {}",
                    Arc::strong_count(curr.as_task_ref())
                );
                axhal::power::system_off();
            }
            CurrentTask::clean_current();
        }
        Poll::Pending => {
            let mut state = curr.state_lock_manual();
            match **state {
                // await 主动让权，将任务的状态修改为就绪后，放入就绪队列中
                TaskState::Running => {
                    if let Some(tf) = curr.utrap_frame() {
                        if tf.trap_status == TrapStatus::Done {
                            tf.kernel_sp = asynctask::current_stack_top();
                            tf.scause = 0;
                            // 这里不能打开中断
                            axhal::asm::disable_irqs();
                            drop(core::mem::ManuallyDrop::into_inner(state));
                            unsafe {
                                tf.user_return();
                            }
                            panic!("never reach here");
                        }
                    }
                    **state = TaskState::Runable;
                    curr.get_scheduler()
                        .lock()
                        .put_prev_task(curr.clone(), false);
                    CurrentTask::clean_current();
                }
                // 处于 Runable 状态的任务一定处于就绪队列中，不可能在 CPU 上运行
                TaskState::Runable => panic!("Runable {} cannot be peding", curr.id_name()),
                // 等待 Mutex 等进入到 Blocking 状态，但还在这个 CPU 上运行，
                // 此时还没有被唤醒，因此将状态修改为 Blocked，等待被唤醒
                TaskState::Blocking => {
                    **state = TaskState::Blocked;
                    CurrentTask::clean_current_without_drop();
                }
                // 由于等待 Mutex 等，导致进入到了 Blocking 状态，但在这里还没有修改状态为 Blocked 时
                // 已经被其他 CPU 上运行的任务唤醒了，因此这里直接返回，让当前的任务继续执行
                TaskState::Waked => {
                    **state = TaskState::Running;
                }
                // Blocked 状态的任务不可能在 CPU 上运行
                TaskState::Blocked => panic!("Blocked {} cannot be pending", curr.id_name()),
                // 退出的任务只能对应到 Poll::Ready
                TaskState::Exited => panic!("Exited {} cannot be pending", curr.id_name()),
            }
            // 在这里释放锁，中间的过程不会发生中断
            drop(core::mem::ManuallyDrop::into_inner(state));
        }
    }
}
