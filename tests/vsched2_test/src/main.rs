use std::{
    process, thread,
    time::{Duration, Instant},
};

unsafe extern "C" {
    fn getauxval(key: libc::c_ulong) -> libc::c_ulong;
}

const AT_SYSINFO_EHDR: libc::c_ulong = 33;

fn fail(message: impl AsRef<str>) -> ! {
    eprintln!("VSCHED2_TEST FAIL: {}", message.as_ref());
    process::exit(1);
}

fn test_user_vdso_mapping() {
    let vdso_base = unsafe { getauxval(AT_SYSINFO_EHDR) } as usize;
    if vdso_base == 0 {
        fail("AT_SYSINFO_EHDR is zero");
    }

    let elf_magic = unsafe { core::slice::from_raw_parts(vdso_base as *const u8, 4) };
    if elf_magic != b"\x7fELF" {
        fail(format!("invalid vDSO ELF magic at {vdso_base:#x}"));
    }

    println!("VSCHED2_TEST user_vdso PASS base={vdso_base:#x}");
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

fn test_minimal_fork_exit() {
    println!("VSCHED2_TEST FORK START");
    let pid = unsafe { libc::fork() };
    if pid == 0 {
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
    println!("VSCHED2_TEST FORK PASS pid={}", pid);
}

fn main() {
    println!("VSCHED2_TEST START");
    test_user_vdso_mapping();
    test_timer_wakeups();
    test_minimal_fork_exit();
    println!("VSCHED2_TEST PASS");
}
