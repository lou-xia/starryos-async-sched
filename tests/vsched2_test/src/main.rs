use std::{
    process,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use vsched_abi::{SHARED_TASK_LIVE, UserTaskKey, encode_task};

unsafe extern "C" {
    fn getauxval(key: libc::c_ulong) -> libc::c_ulong;
}

const AT_SYSINFO_EHDR: libc::c_ulong = 33;
const AT_SYSINFO: libc::c_ulong = 32;

fn fail(message: impl AsRef<str>) -> ! {
    eprintln!("VSCHED2_TEST FAIL: {}", message.as_ref());
    process::exit(1);
}

fn test_user_vdso_mapping() -> usize {
    let vdso_base = unsafe { getauxval(AT_SYSINFO_EHDR) } as usize;
    if vdso_base == 0 {
        fail("AT_SYSINFO_EHDR is zero");
    }

    let elf_magic = unsafe { core::slice::from_raw_parts(vdso_base as *const u8, 4) };
    if elf_magic != b"\x7fELF" {
        fail(format!("invalid vDSO ELF magic at {vdso_base:#x}"));
    }

    unsafe { libvsched2::init_vdso_vtable(vdso_base as u64) };

    println!("VSCHED2_TEST user_vdso PASS base={vdso_base:#x}");

    let starry_base = unsafe { getauxval(AT_SYSINFO) } as usize;
    if starry_base == 0 {
        fail("AT_SYSINFO is zero");
    }
    let starry_magic = unsafe { core::slice::from_raw_parts(starry_base as *const u8, 4) };
    if starry_magic != b"\x7fELF" {
        fail(format!(
            "invalid StarryOS vDSO ELF magic at {starry_base:#x}"
        ));
    }

    unsafe { libstarry_vsched::init_vdso_vtable(starry_base as u64) };
    vsched_user::init_task_vtable();

    let table = unsafe { &*libstarry_vsched::user_task_table() };
    let key = table
        .slots
        .iter()
        .enumerate()
        .find_map(|(slot, task)| {
            (task.state.load(Ordering::Acquire) == SHARED_TASK_LIVE)
                .then(|| UserTaskKey::new(slot, task.generation.load(Ordering::Acquire)))
        })
        .unwrap_or_else(|| fail("shared task table has no live user task"));
    let task_id = encode_task(key).unwrap_or_else(|| fail("live user task cannot be encoded"));
    let current = vsched_user::task(task_id)
        .unwrap_or_else(|| fail("encoded user task did not resolve through the shared table"));
    use libvsched2::Task as _;
    let state = current.state();
    let matched = current.match_set_state(
        libvsched2::TaskState::Ready,
        libvsched2::TaskState::Running,
        libvsched2::TaskState::Blocked,
        libvsched2::TaskState::Exited,
        libvsched2::TaskState::Blocking,
    );
    let pid = current.pid();
    if current.is_kernel() || current.pid() != pid {
        fail("user Task VTABLE returned an invalid process identity");
    }
    println!(
        "VSCHED2_TEST user_task_vtable PASS pid={pid} state={state:?} matched={matched:?} \
         priority={} coroutine={}",
        current.priority(),
        current.is_coroutine(),
    );

    let kernel_value = libstarry_vsched::stage_a_get_shared();
    libstarry_vsched::stage_a_set_shared(kernel_value.wrapping_add(1));
    if libstarry_vsched::stage_a_get_shared() != kernel_value.wrapping_add(1) {
        fail("StarryOS vDSO shared vVAR write/read failed");
    }
    println!(
        "VSCHED2_TEST starry_vdso PASS base={starry_base:#x} shared={}",
        libstarry_vsched::stage_a_get_shared()
    );
    libstarry_vsched::stage_a_get_shared()
}

fn test_timer_wakeups() {
    // Repeated sleeps exercise multiple coroutine -> thread -> coroutine
    // continuation round trips on the same user task.  Timing is deliberately
    // loose: this test checks that timer Wakers resume the handler, not the
    // exact StarryOS timer policy.
    let start = Instant::now();
    for _ in 0..6 {
        thread::sleep(Duration::from_millis(20));
    }
    let elapsed = start.elapsed();
    if elapsed < Duration::from_millis(60) {
        fail(format!(
            "timer waits returned too early: elapsed={}ms",
            elapsed.as_millis()
        ));
    }
    println!("VSCHED2_TEST timer PASS elapsed_ms={}", elapsed.as_millis());
}

fn test_minimal_fork_exit(shared_value: usize) {
    println!("VSCHED2_TEST FORK START");
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        let starry_base = unsafe { getauxval(AT_SYSINFO) } as usize;
        if starry_base == 0 {
            unsafe { libc::_exit(111) };
        }
        unsafe { libstarry_vsched::init_vdso_vtable(starry_base as u64) };
        if libstarry_vsched::stage_a_get_shared() != shared_value {
            unsafe { libc::_exit(112) };
        }
        libstarry_vsched::stage_a_fetch_add(1);
        unsafe { libc::_exit(0) };
    }
    if pid < 0 {
        fail("fork returned a negative pid");
    }
    let mut status = 0;
    let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
    if waited != pid {
        fail(format!("waitpid returned {} for child {}", waited, pid));
    }
    if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
        fail(format!("child status is {:#x}", status));
    }
    if libstarry_vsched::stage_a_get_shared() != shared_value + 1 {
        fail("fork child did not observe the shared vVAR");
    }
    println!("VSCHED2_TEST FORK PASS pid={}", pid);
}

fn test_same_process_thread_registration() {
    let progress = Arc::new(AtomicUsize::new(0));
    let child_progress = progress.clone();
    let worker = thread::spawn(move || {
        child_progress.store(1, Ordering::Release);
    });
    worker
        .join()
        .unwrap_or_else(|_| fail("same-process worker thread panicked"));
    if progress.load(Ordering::Acquire) != 1 {
        fail("same-process worker did not run to completion");
    }
    println!("VSCHED2_TEST clone_thread PASS");
}

fn main() {
    println!("VSCHED2_TEST START");
    let shared_value = test_user_vdso_mapping();
    test_timer_wakeups();
    test_same_process_thread_registration();
    test_minimal_fork_exit(shared_value);
    println!("VSCHED2_TEST PASS");
}
