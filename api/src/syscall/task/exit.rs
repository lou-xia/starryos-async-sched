use axerrno::AxResult;

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
    let task = starry_core::vsched::current_task_ptr();
    if !task.is_null() {
        unsafe { starry_core::vsched::set_vsched_task_exited(task); }
    }
}
