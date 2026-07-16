use alloc::{sync::Arc, task::Wake, vec::Vec};
use core::{
    future::poll_fn,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};

use axerrno::{AxError, AxResult, LinuxError};
use axtask::{
    current,
    future::{block_on, interruptible},
    WeakAxTaskRef,
};
use bitflags::bitflags;
use linux_raw_sys::general::{
    __WALL, __WCLONE, __WNOTHREAD, WCONTINUED, WEXITED, WNOHANG, WNOWAIT, WUNTRACED,
};
use starry_core::task::AsThread;
use starry_process::{Pid, Process};
use starry_vm::{VmMutPtr, VmPtr};

pub(crate) enum WaitPidStep {
    Complete(AxResult<isize>),
    Pending,
}

struct TrapTaskWaker {
    task: usize,
    task_ref: WeakAxTaskRef,
    generation: usize,
    armed: AtomicBool,
    woken: AtomicBool,
    queued: AtomicBool,
}

impl TrapTaskWaker {
    fn new(task: *const (), task_ref: WeakAxTaskRef, generation: usize) -> Self {
        Self {
            task: task as usize,
            task_ref,
            generation,
            armed: AtomicBool::new(false),
            woken: AtomicBool::new(false),
            queued: AtomicBool::new(false),
        }
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::Release);
        self.queue_if_ready();
    }

    fn queue_if_ready(&self) {
        if !self.armed.load(Ordering::Acquire) || !self.woken.load(Ordering::Acquire) {
            return;
        }
        if self.queued.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(_task_ref) = self.task_ref.upgrade() else {
            return;
        };
        if starry_core::vsched::wake_blocked_task(
            self.task as *const (),
            self.generation,
        ) {
            axlog::ax_println!("[wait4] WAKE task={:#x}", self.task);
        }
    }
}

impl Wake for TrapTaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::Release);
        self.queue_if_ready();
    }
}

bitflags! {
    #[derive(Debug)]
    struct WaitOptions: u32 {
        /// Do not block when there are no processes wishing to report status.
        const WNOHANG = WNOHANG;
        /// Report the status of selected processes which are stopped due to a
        /// `SIGTTIN`, `SIGTTOU`, `SIGTSTP`, or `SIGSTOP` signal.
        const WUNTRACED = WUNTRACED;
        /// Report the status of selected processes which have terminated.
        const WEXITED = WEXITED;
        /// Report the status of selected processes that have continued from a
        /// job control stop by receiving a `SIGCONT` signal.
        const WCONTINUED = WCONTINUED;
        /// Don't reap, just poll status.
        const WNOWAIT = WNOWAIT;

        /// Don't wait on children of other threads in this group
        const WNOTHREAD = __WNOTHREAD;
        /// Wait on all children, regardless of type
        const WALL = __WALL;
        /// Wait for "clone" children only.
        const WCLONE = __WCLONE;
    }
}

#[derive(Debug, Clone, Copy)]
enum WaitPid {
    /// Wait for any child process
    Any,
    /// Wait for the child whose process ID is equal to the value.
    Pid(Pid),
    /// Wait for any child process whose process group ID is equal to the value.
    Pgid(Pid),
}

impl WaitPid {
    fn apply(&self, child: &Process) -> bool {
        match self {
            WaitPid::Any => true,
            WaitPid::Pid(pid) => child.pid() == *pid,
            WaitPid::Pgid(pgid) => child.group().pgid() == *pgid,
        }
    }
}

pub fn sys_waitpid_step(pid: i32, exit_code: *mut i32, options: u32) -> WaitPidStep {
    axlog::ax_println!("[wait4] ENTRY pid={} options={:#x}", pid, options);
// axlog::ax_println!("[wait] pid={} options={:?}", pid, options);
    let options = WaitOptions::from_bits_truncate(options);

    let curr = current();
    let proc_data = &curr.as_thread().proc_data;
    let proc = &proc_data.proc;

    let pid = if pid == -1 {
        WaitPid::Any
    } else if pid == 0 {
        WaitPid::Pgid(proc.group().pgid())
    } else if pid > 0 {
        WaitPid::Pid(pid as _)
    } else {
        WaitPid::Pgid(-pid as _)
    };

    // FIXME: add back support for WALL & WCLONE, since ProcessData may drop before
    // Process now.
    let children = proc
        .children()
        .into_iter()
        .filter(|child| pid.apply(child))
        .collect::<Vec<_>>();
    if children.is_empty() {
        return WaitPidStep::Complete(Err(AxError::from(LinuxError::ECHILD)));
    }

    let check_children = || {
        if let Some(child) = children.iter().find(|child| child.is_zombie()) {
            if !options.contains(WaitOptions::WNOWAIT) {
                child.free();
            }
            if let Some(exit_code) = exit_code.nullable() {
                exit_code.vm_write(child.exit_code())?;
            }
            Ok(Some(child.pid() as _))
        } else if options.contains(WaitOptions::WNOHANG) {
            Ok(Some(0))
        } else {
            Ok(None)
        }
    };
// axlog::ax_println!("[wait] blocking...");
    let trapped_task = starry_core::vsched::trapped_vsched_task();
    if axtask::vsched2_active() && !trapped_task.is_null() {
        let task = unsafe {
            &*(trapped_task as *const starry_core::vsched::task::VschedTaskImpl)
        };
        let waiter = Arc::new(TrapTaskWaker::new(
            trapped_task,
            Arc::downgrade(&task.task),
            task.wake_generation.load(Ordering::Acquire),
        ));
        let waker = Waker::from(waiter.clone());
        let mut cx = Context::from_waker(&waker);

        if curr.poll_interrupt(&mut cx).is_ready() {
            return WaitPidStep::Complete(Err(AxError::Interrupted));
        }

        if let Some(result) = check_children().transpose() {
            return WaitPidStep::Complete(result);
        }

        // 先注册Waker再复查条件，覆盖child-exit发生在首次检查与注册之间的竞态。
        proc_data.child_exit_event.register(&waker);
        if let Some(result) = check_children().transpose() {
            return WaitPidStep::Complete(result);
        }

        task.task.set_state(axtask::TaskState::Blocked);
        waiter.arm();
        axlog::ax_println!(
            "[wait4] PENDING task={:#x} children={}",
            trapped_task as usize,
            children.len()
        );
        return WaitPidStep::Pending;
    }

    axlog::ax_println!("[wait4] BLOCK children={}", children.len());

    let result = block_on(interruptible(poll_fn(|cx| {
        match check_children().transpose() {
            Some(res) => Poll::Ready(res),
            None => {
                proc_data.child_exit_event.register(cx.waker());
                Poll::Pending
            }
        }
    })))
    .map_err(AxError::from)
    .and_then(|result| result);
    WaitPidStep::Complete(result)
}
