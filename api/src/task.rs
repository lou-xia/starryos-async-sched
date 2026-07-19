use core::{
    ffi::c_long,
    sync::atomic::{AtomicBool, Ordering},
};

use axerrno::{AxError, AxResult};
use axhal::uspace::{ExceptionKind, ReturnReason, UserContext};
use axtask::{AxTaskRef, TaskInner, current, spawn_task, vsched2_active};
use bytemuck::AnyBitPattern;
use linux_raw_sys::general::ROBUST_LIST_LIMIT;
use ringbuf::Arc;
use starry_core::{
    futex::FutexKey,
    shm::SHM_MANAGER,
    task::{
        AsThread, get_process_data, get_task, send_signal_to_process, send_signal_to_thread,
        set_timer_state,
    },
    time::TimerState,
};
use starry_process::Pid;
use starry_signal::{SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    signal::{check_signals, unblock_next_signal},
    syscall::{SyscallOutcome, handle_syscall},
};

/// Create a new user task.
pub fn new_user_task(name: &str, mut uctx: UserContext, set_child_tid: usize) -> TaskInner {
    TaskInner::new(
        move || {
            let curr = axtask::current();

            if let Some(tid) = (set_child_tid as *mut Pid).nullable() {
                tid.vm_write(curr.id().as_u64() as Pid).ok();
            }

            info!("Enter user space: ip={:#x}, sp={:#x}", uctx.ip(), uctx.sp());

            let thr = curr.as_thread();
            while !thr.pending_exit() {
                let reason = uctx.run();

                set_timer_state(&curr, TimerState::Kernel);

                match reason {
                    ReturnReason::Syscall => {
                        let _ = handle_syscall(&mut uctx);
                    }
                    // ReturnReason::Syscall => {
                    //     let finish = Arc::new(AtomicBool::new(false));
                    //     syscall_task(uctx, finish.clone());
                    //     while !finish.load(Ordering::SeqCst) {
                    //         axtask::yield_now();
                    //     }
                    // }
                    ReturnReason::PageFault(addr, flags) => {
                        if !thr.proc_data.aspace.lock().handle_page_fault(addr, flags) {
                            info!(
                                "{:?}: segmentation fault at {:#x} {:?}",
                                thr.proc_data.proc, addr, flags
                            );
                            raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSEGV))
                                .expect("Failed to send SIGSEGV");
                        }
                    }
                    ReturnReason::Interrupt => {}
                    #[allow(unused_labels)]
                    ReturnReason::Exception(exc_info) => 'exc: {
                        // TODO: detailed handling
                        let signo = match exc_info.kind() {
                            ExceptionKind::Misaligned => {
                                #[cfg(target_arch = "loongarch64")]
                                if unsafe { uctx.emulate_unaligned() }.is_ok() {
                                    break 'exc;
                                }
                                Signo::SIGBUS
                            }
                            ExceptionKind::Breakpoint => Signo::SIGTRAP,
                            ExceptionKind::IllegalInstruction => Signo::SIGILL,
                            _ => Signo::SIGTRAP,
                        };
                        raise_signal_fatal(SignalInfo::new_kernel(signo))
                            .expect("Failed to send SIGTRAP");
                    }
                    r => {
                        warn!("Unexpected return reason: {r:?}");
                        raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSEGV))
                            .expect("Failed to send SIGSEGV");
                    }
                }

                if !unblock_next_signal() {
                    while check_signals(thr, &mut uctx, None) {}
                }

                set_timer_state(&curr, TimerState::User);
                curr.clear_interrupt();
            }
        },
        name.into(),
        starry_core::config::KERNEL_STACK_SIZE,
    )
}

/// Create a vsched2 user task from a TaskInner and UserContext.
/// The TaskInner must already have Thread + proc_data attached via
/// task_ext_mut() (same as the original spawn_task path).
/// Used by sys_clone to spawn child processes under vsched2.
pub fn new_vsched_user_task(
    mut new_task: TaskInner,
    uctx: &UserContext,
) -> (AxTaskRef, *const ()) {
    use alloc::boxed::Box;
    use core::sync::atomic::Ordering;
    use starry_core::vsched::trapframe::{UserTrapFrame, UserTrapFrameKind};

    let entry_ra = new_task.ctx_mut().ra;
    let task_ref = axtask::into_ref(new_task);

    // Get proc_data from the Thread attached by sys_clone
    let thr = task_ref.try_as_thread().expect("vsched2 child must have thread");
    let pid = thr.proc_data.proc.pid() as usize;
    let user_root = thr.proc_data.aspace.lock().page_table_root().as_usize();
    let vdso_base = thr.proc_data.aspace.lock().vdso_base;
    let aspace_mutex_ptr = Arc::as_ptr(&thr.proc_data.aspace) as usize;

    // Copy ALL parent registers (clone/fork semantics: child shares parent state)
    let mut tf = Box::new(UserTrapFrame {
        regs: unsafe { core::mem::transmute_copy(&uctx.regs) },
        sepc: uctx.sepc,
        sstatus: uctx.sstatus.bits(),
        scause: 0,
        stval: 0,
        kind: UserTrapFrameKind::Trap,
    });
    // Child of fork returns 0
    tf.regs.a0 = 0;
    tf.regs.ra = entry_ra;

    let tf_ptr = Box::into_raw(tf);

    let vti = starry_core::vsched::register_task(task_ref.clone(), 0, pid, None, vdso_base);
    let vti_ref = unsafe { &*vti };
    vti_ref.trap_frame.store(tf_ptr as usize, Ordering::Release);
    vti_ref.user_page_table_root.store(user_root, Ordering::Release);
    vti_ref.user_aspace_ptr.store(aspace_mutex_ptr, Ordering::Release);

    let stack_ptr = starry_core::vsched::alloc_stack();
    vti_ref.thread_stack_ptr.store(stack_ptr as usize, Ordering::Release);

    // Caller must push the task to the correct scheduler after process_init
    let vti_raw = vti as *const ();

    (task_ref, vti_raw)
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AnyBitPattern)]
pub struct RobustList {
    pub next: *mut RobustList,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AnyBitPattern)]
pub struct RobustListHead {
    pub list: RobustList,
    pub futex_offset: c_long,
    pub list_op_pending: *mut RobustList,
}

fn handle_futex_death(entry: *mut RobustList, offset: i64) -> AxResult<()> {
    let address = (entry as u64)
        .checked_add_signed(offset)
        .ok_or(AxError::InvalidInput)?;
    let address: usize = address.try_into().map_err(|_| AxError::InvalidInput)?;
    let key = FutexKey::new_current(address);

    let curr = current();
    let futex_table = curr.as_thread().proc_data.futex_table_for(&key);

    let Some(futex) = futex_table.get(&key) else {
        return Ok(());
    };
    futex.owner_dead.store(true, Ordering::SeqCst);
    futex.wq.wake(1, u32::MAX);
    Ok(())
}

pub fn exit_robust_list(head: *const RobustListHead) -> AxResult<()> {
    // Reference: https://elixir.bootlin.com/linux/v6.13.6/source/kernel/futex/core.c#L777

    let mut limit = ROBUST_LIST_LIMIT;

    let end_ptr = unsafe { &raw const (*head).list };
    let head = head.vm_read()?;
    let mut entry = head.list.next;
    let offset = head.futex_offset;
    let pending = head.list_op_pending;

    while !core::ptr::eq(entry, end_ptr) {
        let next_entry = entry.vm_read()?.next;
        if entry != pending {
            handle_futex_death(entry, offset)?;
        }
        entry = next_entry;

        limit -= 1;
        if limit == 0 {
            return Err(AxError::FilesystemLoop);
        }
        axtask::yield_now();
    }

    Ok(())
}

pub fn do_exit(exit_code: i32, group_exit: bool) {
    let curr = current();
    let thr = curr.as_thread();

    info!("{} exit with code: {}", curr.id_name(), exit_code);

    let clear_child_tid = thr.clear_child_tid() as *mut u32;
    if clear_child_tid.vm_write(0).is_ok() {
        let key = FutexKey::new_current(clear_child_tid as usize);
        let table = thr.proc_data.futex_table_for(&key);
        let guard = table.get(&key);
        if let Some(futex) = guard {
            futex.wq.wake(1, u32::MAX);
        }
        if !vsched2_active() {
            axtask::yield_now();
        }
    }
    let head = thr.robust_list_head() as *const RobustListHead;
    if !head.is_null()
        && let Err(err) = exit_robust_list(head)
    {
        warn!("exit robust list failed: {err:?}");
    }

    let process = &thr.proc_data.proc;
    let last_thread = process.exit_thread(curr.id().as_u64() as Pid, exit_code);
    if last_thread {
        process.exit();
        if let Some(parent) = process.parent() {
            if let Some(signo) = thr.proc_data.exit_signal {
                let _ = send_signal_to_process(parent.pid(), Some(SignalInfo::new_kernel(signo)));
            }
            if let Ok(data) = get_process_data(parent.pid()) {
                data.child_exit_event.wake();
            }
        }
        thr.proc_data.exit_event.wake();

        SHM_MANAGER.lock().clear_proc_shm(process.pid());
    }
    if group_exit && !process.is_group_exited() {
        process.group_exit();
        let sig = SignalInfo::new_kernel(Signo::SIGKILL);
        for tid in process.threads() {
            let _ = send_signal_to_thread(None, tid, Some(sig.clone()));
        }
    }
    thr.set_exit();
}

/// Sends a fatal signal to the given task's process.
pub fn raise_signal_fatal_for_task(task: &AxTaskRef, sig: SignalInfo) -> AxResult<()> {
    let Some(thr) = task.try_as_thread() else {
        // Kernel/handler task has no process — find the last user task.
        let last_user = starry_core::vsched::trap::get_last_trapped_user_task();
        if !last_user.is_null() {
            let last_task = unsafe { &*last_user }.task.clone();
            return raise_signal_fatal_for_task(&last_task, sig);
        }
        warn!("raise_signal_fatal_for_task: no thread, no last user — dropping signal {:?}", sig.signo());
        return Ok(());
    };
    let proc_data = &thr.proc_data;
    let signo = sig.signo();
    info!("Send fatal signal {signo:?} to the current process");
    if let Some(tid) = proc_data.signal.send_signal(sig)
        && let Ok(t) = get_task(tid)
    {
        t.interrupt();
    } else {
        do_exit(signo as i32, true);
    }
    Ok(())
}

/// Sends a fatal signal to the current process.
pub fn raise_signal_fatal(sig: SignalInfo) -> AxResult<()> {
    raise_signal_fatal_for_task(&current(), sig)
}

fn syscall_task(mut uctx: UserContext, finish: Arc<AtomicBool>) {
    let entry = move || {
        assert!(
            !finish.load(Ordering::SeqCst),
            "Syscall task should only run once"
        );
        handle_syscall(&mut uctx);
        finish.store(true, Ordering::SeqCst);
    };
    let task = TaskInner::new(
        entry,
        "syscall".into(),
        starry_core::config::KERNEL_STACK_SIZE,
    );
    spawn_task(task);
}

use starry_core::vsched::trapframe::UserTrapFrame;
use starry_core::vsched::task::VschedTaskImpl;

use axhal::paging::MappingFlags;
use memory_addr::VirtAddr;

fn vsched_trap_dispatcher(trapped_task: *const VschedTaskImpl) {
    let vti = unsafe { &*trapped_task };
    let tf_ptr = vti.trap_frame.load(Ordering::Acquire);
    if tf_ptr == 0 {
        return;
    }
    let tf = unsafe { &*(tf_ptr as *const UserTrapFrame) };

    // Use the TRAPPED task for signals, not current() (which is trap handler)
    let mut signal_task = vti.task.clone();
    // If trapped task is a kernel coroutine (handler), use last user task for signals
    if !vti.task.try_as_thread().is_some() {
        let last_user = starry_core::vsched::trap::get_last_trapped_user_task();
        if !last_user.is_null() {
            signal_task = unsafe { &*last_user }.task.clone();
        }
    }

    // For page faults during syscall handling, the trapped task is the handler.
    // Store the actual user task being serviced to use for page fault resolution.
    static LAST_ECALL_USER_TASK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

    match tf.scause {
        // Timer interrupt: handled by stub (stimecmp reset), just ignore here
// if n < 5 { axlog::ax_println!("[stats] timer={}", n+1); }
        sc if sc >> 63 == 1 => {}
// axlog::ax_println!("[ecall#{}] a7={} a0={:#x} a1={:#x} a2={}", n, a7, tf.regs.a0, tf.regs.a1, tf.regs.a2);
          8 => {
            let mut uctx = UserContext::new(
                tf.sepc + 4,
                VirtAddr::from(tf.regs.sp),
                tf.regs.a0,
            );
            uctx.set_arg1(tf.regs.a1);
            uctx.set_arg2(tf.regs.a2);
            uctx.set_arg3(tf.regs.a3);
            uctx.set_arg4(tf.regs.a4);
            uctx.set_arg5(tf.regs.a5);
            uctx.set_sysno(tf.regs.a7);
            uctx.set_ra(tf.regs.ra);
            uctx.set_tls(tf.regs.tp);

            // Store user task for page fault fallback
            LAST_ECALL_USER_TASK.store(trapped_task as usize, Ordering::Release);

            // Set the active scope to the user task's scope so
            // scope-local variables (FD_TABLE etc.) resolve correctly.
            let scope_guard = vti.task.try_as_thread().map(|thr| {
                let guard = thr.proc_data.scope.read();
                // SAFETY: guard holds the lock; scope lives until guard drops.
                unsafe { scope_local::ActiveScope::set(&*guard) };
                guard
            });
            // 记录被服务的用户任务，供 mark_exited 使用。
            // 不覆盖 vsched2 CURRENT_TASK（仍是 trap_handler），
            // 确保 yield 时从 handler 身份走 run_task 路径。
            starry_core::vsched::set_trapped_vsched_task(trapped_task as *const ());

            let syscall_outcome = axtask::with_current_task(&vti.task, || {
                handle_syscall(&mut uctx)
            });

            drop(scope_guard);
            scope_local::ActiveScope::set_global();

            if syscall_outcome == SyscallOutcome::Pending {
                return;
            }

            let tf_mut = unsafe { &mut *(tf_ptr as *mut UserTrapFrame) };
            let new_ip = uctx.ip();
            let new_sp = uctx.regs.sp;
// axlog::ax_println!(

            tf_mut.regs.a0 = uctx.arg0();  // return value
            tf_mut.regs.a1 = uctx.arg1();
            tf_mut.regs.sp = new_sp;
            tf_mut.sepc = new_ip;
// axlog::ax_println!("[write] fd={} len={} ret={}", tf.regs.a0, tf.regs.a2, tf_mut.regs.a0);
            tf_mut.sstatus = uctx.sstatus.bits();
        }
// if log_detail { axlog::ax_println!("[pf] ENTER vaddr={:#x} scause={}", tf.stval, tf.scause); }
          12 | 13 | 15 => {
            let vaddr = VirtAddr::from(tf.stval);
            let flags = match tf.scause {
                12 => MappingFlags::EXECUTE | MappingFlags::USER,
        13 => MappingFlags::READ | MappingFlags::USER,
        _ => MappingFlags::WRITE | MappingFlags::USER,
            };
            // Try trapped task first, then fall back to axtask::current()
// if log_detail { axlog::ax_println!("[pf] try_as_thread: vti={}", vti.task.try_as_thread().is_some()); }
            let mut fixed = {
                if let Some(thr) = vti.task.try_as_thread() {
// if log_detail { axlog::ax_println!("[pf] vaddr={:#x} PT query={:?}", vaddr.as_usize(), pt_result); }
                    let mut aspace = thr.proc_data.aspace.lock();
                    aspace.handle_page_fault(vaddr, flags)
                } else {
// axlog::ax_println!("[pf] fallback: axcur has_thread={}", cur.try_as_thread().is_some());
                    let cur = axtask::current();
                    if let Some(thr) = cur.try_as_thread() {
                        let mut aspace = thr.proc_data.aspace.lock();
                        aspace.handle_page_fault(vaddr, flags)
                    } else {
                        let last_user = starry_core::vsched::trap::get_last_trapped_user_task();
                        if !last_user.is_null() {
                            let last_vti = unsafe { &*last_user };
                            if let Some(thr) = last_vti.task.try_as_thread() {
                                let mut aspace = thr.proc_data.aspace.lock();
                                // Check if page is already mapped before handle_page_fault
                                if aspace.page_table().query(vaddr).is_ok() {
                                    true
                                } else {
                                    let hpf = aspace.handle_page_fault(vaddr, flags);
                                    hpf
                                }
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    }
                }
// axlog::ax_println!("[pf] vaddr={:#x} fixed={} scause={}", tf.stval, fixed, tf.scause);
            };
            if !fixed {
                raise_signal_fatal_for_task(&signal_task, SignalInfo::new_kernel(Signo::SIGSEGV)).ok();
            }
        }
// axlog::ax_println!("[ecall] a7={} a0={} a1={:#x} a2={} sepc={:#x} sp={:#x}",
//          8 => {
//             let mut uctx = UserContext::new(
//                 tf.sepc + 4,
//                 VirtAddr::from(tf.regs.sp),
//                 tf.regs.a0,
//             );
//             uctx.set_arg1(tf.regs.a1);
//             uctx.set_arg2(tf.regs.a2);
//             uctx.set_arg3(tf.regs.a3);
//             uctx.set_arg4(tf.regs.a4);
//             uctx.set_arg5(tf.regs.a5);
//             uctx.set_sysno(tf.regs.a7);
//             uctx.set_ra(tf.regs.ra);
//             uctx.set_tls(tf.regs.tp);

//             // Store user task for page fault fallback
//             LAST_ECALL_USER_TASK.store(trapped_task as usize, Ordering::Release);

//             axtask::with_current_task(&vti.task, || {
//                 handle_syscall(&mut uctx);
//             });

//             let tf_mut = unsafe { &mut *(tf_ptr as *mut UserTrapFrame) };
//             // Only update caller-saved registers that syscall may change.
//             // Keep callee-saved regs (s0-s11) from the original trap frame.
//             tf_mut.regs.a0 = uctx.arg0();  // return value
//             tf_mut.regs.a1 = uctx.arg1();  // may be modified (pipe, etc.)
//             // a2-a5: keep original (not usually modified by kernel)
//             // a6-a7: keep original
//             tf_mut.sepc = uctx.ip();
// // axlog::ax_println!("[ecall] ret a0={} new_sepc={:#x}", tf_mut.regs.a0, tf_mut.sepc);
//             tf_mut.sstatus = uctx.sstatus.bits();
//         }
        1 | 5 | 7 => {
            axlog::error!(
                "vsched trap: memory access fault scause={}, vaddr={:#x}, task={}",
                tf.scause,
                tf.stval,
                vti.task.id_name(),
            );
            raise_signal_fatal_for_task(&signal_task, SignalInfo::new_kernel(Signo::SIGSEGV)).ok();
        }
         2 => {
            axlog::error!(
                "vsched trap: illegal instruction @ {:#x}, task={}",
                tf.sepc,
                vti.task.id_name(),
            );
            raise_signal_fatal_for_task(&signal_task, SignalInfo::new_kernel(Signo::SIGILL)).ok();
        }
         3 => {
            raise_signal_fatal_for_task(&signal_task, SignalInfo::new_kernel(Signo::SIGTRAP)).ok();
        }
         0 | 4 | 6 => {
            axlog::error!(
                "vsched trap: misaligned access scause={}, vaddr={:#x}, task={}",
                tf.scause,
                tf.stval,
                vti.task.id_name(),
            );
            raise_signal_fatal_for_task(&signal_task, SignalInfo::new_kernel(Signo::SIGBUS)).ok();
        }
        _ => {
            axlog::warn!(
                "vsched trap handler: unhandled scause={}, stval={:#x}, sepc={:#x}, task={}",
                tf.scause,
                tf.stval,
                tf.sepc,
                vti.task.id_name(),
            );
            raise_signal_fatal_for_task(&signal_task, SignalInfo::new_kernel(Signo::SIGTRAP)).ok();
        }
    }
}

pub fn register_vsched_trap_dispatcher() {
    starry_core::vsched::trap::register_trap_dispatcher(vsched_trap_dispatcher);
}
