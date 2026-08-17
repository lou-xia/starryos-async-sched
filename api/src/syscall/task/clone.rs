use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::Ordering;

use axerrno::{AxError, AxResult};
use axfs::FS_CONTEXT;
use axhal::uspace::UserContext;
use axtask::{AxTaskExt, current, spawn_task, vsched2_active};
use bitflags::bitflags;
use kspin::SpinNoIrq;
use linux_raw_sys::general::*;
use starry_core::{
    mm::copy_from_kernel,
    task::{AsThread, ProcessData, Thread, add_task_to_table},
};
use starry_process::Pid;
use starry_signal::Signo;
use starry_vm::VmMutPtr;

use crate::{
    file::{FD_TABLE, FileLike, PidFd},
    task::new_user_task,
};

bitflags! {
    /// Options for use with [`sys_clone`].
    #[derive(Debug, Clone, Copy, Default)]
    struct CloneFlags: u32 {
        /// The calling process and the child process run in the same
        /// memory space.
        const VM = CLONE_VM;
        /// The caller and the child process share the same  filesystem
        /// information.
        const FS = CLONE_FS;
        /// The calling process and the child process share the same file
        /// descriptor table.
        const FILES = CLONE_FILES;
        /// The calling process and the child process share the same table
        /// of signal handlers.
        const SIGHAND = CLONE_SIGHAND;
        /// Sets pidfd to the child process's PID file descriptor.
        const PIDFD = CLONE_PIDFD;
        /// If the calling process is being traced, then trace the child
        /// also.
        const PTRACE = CLONE_PTRACE;
        /// The execution of the calling process is suspended until the
        /// child releases its virtual memory resources via a call to
        /// execve(2) or _exit(2) (as with vfork(2)).
        const VFORK = CLONE_VFORK;
        /// The parent of the new child  (as returned by getppid(2))
        /// will be the same as that of the calling process.
        const PARENT = CLONE_PARENT;
        /// The child is placed in the same thread group as the calling
        /// process.
        const THREAD = CLONE_THREAD;
        /// The cloned child is started in a new mount namespace.
        const NEWNS = CLONE_NEWNS;
        /// The child and the calling process share a single list of System
        /// V semaphore adjustment values
        const SYSVSEM = CLONE_SYSVSEM;
        /// The TLS (Thread Local Storage) descriptor is set to tls.
        const SETTLS = CLONE_SETTLS;
        /// Store the child thread ID in the parent's memory.
        const PARENT_SETTID = CLONE_PARENT_SETTID;
        /// Clear (zero) the child thread ID in child memory when the child
        /// exits, and do a wakeup on the futex at that address.
        const CHILD_CLEARTID = CLONE_CHILD_CLEARTID;
        /// A tracing process cannot force `CLONE_PTRACE` on this child
        /// process.
        const UNTRACED = CLONE_UNTRACED;
        /// Store the child thread ID in the child's memory.
        const CHILD_SETTID = CLONE_CHILD_SETTID;
        /// Create the process in a new cgroup namespace.
        const NEWCGROUP = CLONE_NEWCGROUP;
        /// Create the process in a new UTS namespace.
        const NEWUTS = CLONE_NEWUTS;
        /// Create the process in a new IPC namespace.
        const NEWIPC = CLONE_NEWIPC;
        /// Create the process in a new user namespace.
        const NEWUSER = CLONE_NEWUSER;
        /// Create the process in a new PID namespace.
        const NEWPID = CLONE_NEWPID;
        /// Create the process in a new network namespace.
        const NEWNET = CLONE_NEWNET;
        /// The new process shares an I/O context with the calling process.
        const IO = CLONE_IO;
    }
}

pub fn sys_clone(
    uctx: &UserContext,
    flags: u32,
    stack: usize,
    parent_tid: usize,
    #[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))] child_tid: usize,
    tls: usize,
    #[cfg(not(any(target_arch = "x86_64", target_arch = "loongarch64")))] child_tid: usize,
) -> AxResult<isize> {
    axlog::info!(
        "[clone] ENTRY flags={:#x} stack={:#x} ptid={:#x} ctid={:#x} tls={:#x}",
        flags,
        stack,
        parent_tid,
        child_tid,
        tls
    );
    const FLAG_MASK: u32 = 0xff;
    let exit_signal = flags & FLAG_MASK;
    let mut flags = CloneFlags::from_bits_truncate(flags & !FLAG_MASK);
    if flags.contains(CloneFlags::VFORK) {
        debug!("sys_clone: CLONE_VFORK slow path");
        flags.remove(CloneFlags::VM);
    }

    debug!(
        "sys_clone <= flags: {flags:?}, exit_signal: {exit_signal}, stack: {stack:#x}, ptid: \
         {parent_tid:#x}, ctid: {child_tid:#x}, tls: {tls:#x}"
    );

    if exit_signal != 0 && flags.contains(CloneFlags::THREAD | CloneFlags::PARENT) {
        return Err(AxError::InvalidInput);
    }
    if flags.contains(CloneFlags::THREAD) && !flags.contains(CloneFlags::VM | CloneFlags::SIGHAND) {
        return Err(AxError::InvalidInput);
    }
    if flags.contains(CloneFlags::PIDFD | CloneFlags::PARENT_SETTID) {
        return Err(AxError::InvalidInput);
    }
    let exit_signal = Signo::from_repr(exit_signal as u8);

    let mut new_uctx = *uctx;
    if stack != 0 {
        new_uctx.set_sp(stack);
    }
    if flags.contains(CloneFlags::SETTLS) {
        new_uctx.set_tls(tls);
    }
    new_uctx.set_retval(0);

    let set_child_tid = if flags.contains(CloneFlags::CHILD_SETTID) {
        child_tid
    } else {
        0
    };

    let curr = current();
    let old_proc_data = &curr.as_thread().proc_data;

    let mut new_task = new_user_task(&curr.name(), new_uctx, set_child_tid);

    let tid = new_task.id().as_u64() as Pid;
    if flags.contains(CloneFlags::PARENT_SETTID) {
        (parent_tid as *mut Pid).vm_write(tid).ok();
    }

    let new_proc_data = if flags.contains(CloneFlags::THREAD) {
        new_task
            .ctx_mut()
            .set_page_table_root(old_proc_data.aspace.lock().page_table_root());
        old_proc_data.clone()
    } else {
        let proc = if flags.contains(CloneFlags::PARENT) {
            old_proc_data.proc.parent().ok_or(AxError::InvalidInput)?
        } else {
            old_proc_data.proc.clone()
        }
        .fork(tid);

        let aspace = if flags.contains(CloneFlags::VM) {
            old_proc_data.aspace.clone()
        } else {
            let mut aspace = old_proc_data.aspace.lock();
            let aspace = aspace.try_clone()?;
            copy_from_kernel(&mut aspace.lock())?;
            aspace
        };
        new_task
            .ctx_mut()
            .set_page_table_root(aspace.lock().page_table_root());

        let signal_actions = if flags.contains(CloneFlags::SIGHAND) {
            old_proc_data.signal.actions.clone()
        } else {
            Arc::new(SpinNoIrq::new(old_proc_data.signal.actions.lock().clone()))
        };
        let proc_data = ProcessData::new(
            proc,
            old_proc_data.exe_path.read().clone(),
            old_proc_data.cmdline.read().clone(),
            aspace,
            signal_actions,
            exit_signal,
        );
        proc_data.set_umask(old_proc_data.umask());
        // Inherit heap pointers from parent to ensure child's heap state is consistent after fork
        proc_data.set_heap_top(old_proc_data.get_heap_top());

        {
            let mut scope = proc_data.scope.write();
            if flags.contains(CloneFlags::FILES) {
                FD_TABLE.scope_mut(&mut scope).clone_from(&FD_TABLE);
            } else {
                FD_TABLE
                    .scope_mut(&mut scope)
                    .write()
                    .clone_from(&FD_TABLE.read());
            }

            if flags.contains(CloneFlags::FS) {
                FS_CONTEXT.scope_mut(&mut scope).clone_from(&FS_CONTEXT);
            } else {
                FS_CONTEXT
                    .scope_mut(&mut scope)
                    .lock()
                    .clone_from(&FS_CONTEXT.lock());
            }
        }

        proc_data
    };

    new_proc_data.proc.add_thread(tid);

    if flags.contains(CloneFlags::PIDFD) {
        let pidfd = PidFd::new(&new_proc_data);
        (parent_tid as *mut i32).vm_write(pidfd.add_to_fd_table(true)?)?;
    }

    if vsched2_active() {
        // vsched2 is the active scheduler: create a vsched2 task instead
        // of pushing to the legacy AxRunQueue (which is dormant under vsched2).
        let thr = Thread::new(tid, new_proc_data);
        if flags.contains(CloneFlags::CHILD_CLEARTID) {
            thr.set_clear_child_tid(child_tid);
        }
        *new_task.task_ext_mut() = Some(unsafe { AxTaskExt::from_impl(thr) });
        let (task, vti_ptr) = crate::task::new_vsched_user_task(new_task, &new_uctx);

        if !flags.contains(CloneFlags::THREAD) {
            let thr = task
                .try_as_thread()
                .expect("vsched2 child must have thread");
            let saved_root = axhal::asm::read_user_page_table();
            let (child_root, vspace): (_, *mut ()) = {
                let guard = thr.proc_data.aspace.lock();
                let root = guard.page_table_root();
                let p: *mut () = &raw const *guard as *mut ();
                (root, p)
            };

            // Give the child its own vDSO/VVAR pages.  After
            // try_clone(), LinearBackend maps parent and child to
            // the same physical pages.  We unmap the old region and
            // re-create it via map_so(), which gives the child its
            // own .data/.bss pages with a fresh zero-filled bss
            // (identical to load_user_app creation).
            if !flags.contains(CloneFlags::VM) {
                let (user_vdso_base, vvar_size, vdso_size, user_starry_vdso_base, starry_vvar_size, starry_vdso_size) = {
                    let guard = thr.proc_data.aspace.lock();
                    let base = guard.vdso_base;
                    (
                        base,
                        unsafe { starry_core::vsched::VSCHED2_VVAR_SIZE },
                        unsafe { starry_core::vsched::VSCHED2_VDSO_SIZE },
                        guard.starry_vdso_base,
                        unsafe { starry_core::vsched::STARRY_VVAR_SIZE },
                        unsafe { starry_core::vsched::STARRY_VDSO_SIZE },
                    )
                };
                assert_ne!(
                    user_starry_vdso_base, 0,
                    "clone: parent StarryOS vDSO base is not initialized"
                );
                let ranges = [
                    (
                        user_vdso_base - vvar_size,
                        user_vdso_base + vdso_size,
                    ),
                    (
                        user_starry_vdso_base - starry_vvar_size,
                        user_starry_vdso_base + starry_vdso_size,
                    ),
                ];

                {
                    let mut guard = thr.proc_data.aspace.lock();
                    let to_unmap: Vec<_> = guard
                        .areas()
                        .filter(|a| {
                            let s = a.start().as_usize();
                            let e = a.end().as_usize();
                            ranges.iter().any(|(start, end)| s >= *start && e <= *end)
                        })
                        .map(|a| (a.start(), a.end()))
                        .collect();
                    for (start, end) in to_unmap {
                        guard
                            .unmap(start, end - start)
                            .expect("clone: unmap vdso area failed");
                    }
                }

                let new_vdso = starry_core::vsched::map_vdso_for_child(vspace);
                let new_starry_vdso = starry_core::vsched::map_starry_vdso_for_child(vspace);
                let mut aspace = thr.proc_data.aspace.lock();
                aspace.vdso_base = new_vdso as usize;
                aspace.starry_vdso_base = new_starry_vdso;
            }

            let aspace_vdso_base = thr.proc_data.aspace.lock().vdso_base;
            assert_ne!(
                aspace_vdso_base, 0,
                "clone: child vDSO remapping produced a zero base"
            );
            // new_vsched_user_task() runs before a private fork child remaps
            // its vDSO.  Publish the final address before the child becomes
            // runnable so Context::into_user() uses the child mapping.
            starry_core::vsched::with_vsched_task(vti_ptr, |vti| {
                vti.user_vdso_base
                    .store(aspace_vdso_base, Ordering::Release);
            })
            .expect("clone: child pointer is not a vsched2 task");

            let child_pid = starry_core::vsched::process_init(vspace);
            let old_binding = starry_core::vsched::bind_user_process(
                vti_ptr as *const starry_core::vsched::VschedTaskImpl,
                child_pid,
            );
            assert!(
                old_binding.is_none(),
                "clone: new address space already had a vsched process id",
            );
            starry_core::vsched::user_init_with_vspace(vspace);
            let (task_vdso, task_frame) = starry_core::vsched::with_vsched_task(vti_ptr, |vti| {
                (
                    vti.user_vdso_base.load(Ordering::Acquire),
                    vti.trap_frame.load(Ordering::Acquire),
                )
            })
            .expect("clone: child pointer is not a vsched2 task");
            axlog::info!(
                "[vsched2-diag] clone child parent_task={} child_task={:#x} vspace={:#x} saved_root={:#x} child_root={:#x} current_root={:#x} aspace_vdso={:#x} task_vdso={:#x} tf={:#x} sepc={:#x} sp={:#x} gp={:#x} tp={:#x} a0={:#x} sstatus={:#x}",
                curr.id().as_u64(),
                vti_ptr as usize,
                vspace as usize,
                saved_root.as_usize(),
                child_root.as_usize(),
                axhal::asm::read_user_page_table().as_usize(),
                aspace_vdso_base,
                task_vdso,
                task_frame,
                new_uctx.ip(),
                new_uctx.sp(),
                new_uctx.regs.gp,
                new_uctx.regs.tp,
                0usize,
                new_uctx.sstatus.bits(),
            );
            let pushed = starry_core::vsched::push_task_into_process(vti_ptr, child_pid);
            axlog::info!("[clone] push_task pid={}, ok={}", child_pid, pushed);
        } else {
            // CLONE_THREAD 共享 ProcessData，因此会继承父地址空间注册得到的同一个 vsched2
            // 进程表索引。新任务必须进入该地址空间已有的 USER_SCHEDULER；不能把 Linux
            // pid/tid 当作调度器索引。
            let vsched_pid = task
                .as_thread()
                .proc_data
                .vsched_process_id()
                .expect("clone: CLONE_THREAD parent has no vsched process id");
            starry_core::vsched::with_vsched_task(vti_ptr, |vti| {
                assert_eq!(
                    vti.pid.load(Ordering::Acquire),
                    vsched_pid.as_raw(),
                    "clone: CLONE_THREAD task did not inherit the process scheduler",
                );
            })
            .expect("clone: thread pointer is not a vsched2 task");
            // CLONE_THREAD 复用父地址空间的 vsched2 process id，但任务对象仍需拥有
            // 独立的共享槽和 generation。必须先完成 registry 注册，再入就绪队列，
            // 否则多核下子线程可能在注册前已经运行并退出。
            starry_core::vsched::register_user_task(
                vti_ptr as *const starry_core::vsched::VschedTaskImpl,
                vsched_pid,
            );
            assert!(
                starry_core::vsched::push_task_into_process(
                    vti_ptr,
                    vsched_pid.as_raw(),
                ),
                "clone: shared USER_SCHEDULER ready queue is full",
            );
        }

        add_task_to_table(&task);
    } else {
        let thr = Thread::new(tid, new_proc_data);
        if flags.contains(CloneFlags::CHILD_CLEARTID) {
            thr.set_clear_child_tid(child_tid);
        }
        *new_task.task_ext_mut() = Some(unsafe { AxTaskExt::from_impl(thr) });
        let task = spawn_task(new_task);
        add_task_to_table(&task);
    };

    Ok(tid as _)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_fork(uctx: &UserContext) -> AxResult<isize> {
    sys_clone(uctx, SIGCHLD, 0, 0, 0, 0)
}
