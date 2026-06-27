use alloc::{boxed::Box, string::ToString, sync::Arc, vec::Vec};
use core::{ffi::c_char, sync::atomic::Ordering};

use axerrno::{AxError, AxResult};
use axfs::FS_CONTEXT;
use axhal::uspace::UserContext;
use axtask::current;
use starry_core::{config::USER_HEAP_BASE, mm::load_user_app, task::AsThread};
use starry_vm::vm_load_until_nul;

use crate::{file::FD_TABLE, mm::vm_load_string};

pub fn sys_execve(
    uctx: &mut UserContext,
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> AxResult<isize> {
    axlog::ax_println!("[execve] loading path...");
    let path = vm_load_string(path)?;
    axlog::ax_println!("[execve] path={}", path);

    let args = if argv.is_null() {
        Vec::new()
    } else {
        vm_load_until_nul(argv)?
            .into_iter()
            .map(vm_load_string)
            .collect::<Result<Vec<_>, _>>()?
    };

    let envs = if envp.is_null() {
        Vec::new()
    } else {
        vm_load_until_nul(envp)?
            .into_iter()
            .map(vm_load_string)
            .collect::<Result<Vec<_>, _>>()?
    };

    debug!("sys_execve <= path: {path:?}, args: {args:?}, envs: {envs:?}");

    let curr = current();
    let proc_data = &curr.as_thread().proc_data;

    if proc_data.proc.threads().len() > 1 {
        error!("sys_execve: multi-thread not supported");
        return Err(AxError::WouldBlock);
    }

    axlog::ax_println!("[execve] calling load_user_app for {}", path);
    let mut aspace = proc_data.aspace.lock();
    match load_user_app(&mut aspace, Some(path.as_str()), &args, &envs) {
        Ok((entry_point, user_stack_base)) => {
            axlog::ax_println!("[execve] load_user_app OK entry={:#x} sp={:#x}",
                entry_point.as_usize(), user_stack_base.as_usize());
            let vspace_ptr = {
                let p: *mut () = &raw const *aspace as *mut ();
                Box::into_raw(Box::new(p))
            };

            // execve replaces the address space; re-init the vsched2 scheduler,
            // re-using the existing pid (current_task_ptr() returns the trapped task).
            let task_ptr = starry_core::vsched::current_task_ptr();
            let pid = unsafe { &*(task_ptr as *const starry_core::vsched::VschedTaskImpl) }
                .pid.load(Ordering::Acquire);
            starry_core::vsched::process_reinit(vspace_ptr, pid);
            starry_core::vsched::user_init_with_vspace(unsafe { *vspace_ptr });
            drop(aspace);

            let loc = FS_CONTEXT.lock().resolve(&path)?;
            curr.set_name(loc.name());

            *proc_data.exe_path.write() = loc.absolute_path()?.to_string();
            *proc_data.cmdline.write() = Arc::new(args);

            proc_data.set_heap_top(USER_HEAP_BASE);
            *proc_data.signal.actions.lock() = Default::default();
            curr.as_thread().set_clear_child_tid(0);

            let mut fd_table = FD_TABLE.write();
            let cloexec_fds = fd_table
                .ids()
                .filter(|it| fd_table.get(*it).unwrap().cloexec)
                .collect::<Vec<_>>();
            for fd in cloexec_fds {
                fd_table.remove(fd);
            }
            drop(fd_table);

            uctx.set_ip(entry_point.as_usize());
            uctx.set_sp(user_stack_base.as_usize());
            axlog::ax_println!("[execve] done, returning 0");
            Ok(0)
        }
        Err(e) => {
            drop(aspace);
            axlog::ax_println!("[execve] load_user_app FAILED: {:?}", e);
            Err(e)
        }
    }
}
