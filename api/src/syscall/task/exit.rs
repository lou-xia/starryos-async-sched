use axerrno::AxResult;
use axtask::vsched2_active;

use crate::task::do_exit;

pub fn sys_exit(exit_code: i32) -> AxResult<isize> {
    do_exit(exit_code << 8, false);
    mark_exited();
    Ok(0)
}

pub fn sys_exit_group(exit_code: i32) -> AxResult<isize> {
    do_exit(exit_code << 8, true);
    mark_exited();
    Ok(0)
}

fn mark_exited() {
    if !axtask::vsched2_active() {
        return;
    }
    let task = starry_core::vsched::trapped_vsched_task();
    if !task.is_null() {
        unsafe {
            starry_core::vsched::set_vsched_task_exited(task);
        }
    }
}
