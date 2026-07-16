use alloc::{string::ToString, sync::Arc, vec::Vec};
use core::ffi::c_char;

use axerrno::{AxError, AxResult};
use axfs::FS_CONTEXT;
use axhal::uspace::UserContext;
use axtask::current;
use axtask::vsched2_active;
use starry_core::{config::USER_HEAP_BASE, mm::load_user_app, task::AsThread};
use starry_vm::vm_load_until_nul;

use crate::{file::FD_TABLE, mm::vm_load_string};

pub fn sys_execve(
    uctx: &mut UserContext,
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
// axlog::ax_println!("[execve] loading path...");
) -> AxResult<isize> {
// axlog::ax_println!("[execve] path={}", path);
    let path = vm_load_string(path)?;
    axlog::ax_println!("[execve] ENTRY pid={} path={}", current().id().as_u64(), path);

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
// axlog::ax_println!("[execve] calling load_user_app for {}", path);

    let mut aspace = proc_data.aspace.lock();
    match load_user_app(&mut aspace, Some(path.as_str()), &args, &envs) {
// axlog::ax_println!("[execve] load_user_app OK entry={:#x} sp={:#x}",
        Ok((entry_point, user_stack_base)) => {
            let vspace = &raw const *aspace as *mut ();

            // execve replaces the vDSO private data, so create a scheduler in the
            // new address space with the existing vsched2 process lifecycle APIs.
            if vsched2_active() {
                // Switch to new user PT so user_init() targets correct vDSO
                let root = aspace.page_table_root();
                let kernel_root = unsafe { axhal::asm::read_user_page_table() };
                if root.as_usize() != 0 && root != kernel_root {
                    unsafe {
                        axhal::asm::write_user_page_table(root);
                        core::arch::asm!("sfence.vma");
                        riscv::register::sstatus::set_sum();
                    }
                }
                let trapped = starry_core::vsched::trapped_vsched_task()
                    as *const starry_core::vsched::task::VschedTaskImpl;
                assert!(!trapped.is_null(), "execve: no trapped vsched task");
                let task = unsafe { &*trapped };
                let old_pid = task.pid.load(core::sync::atomic::Ordering::Acquire);
                let new_pid = starry_core::vsched::process_init(vspace);
                starry_core::vsched::user_init_with_vspace(vspace);
                task.pid.store(new_pid, core::sync::atomic::Ordering::Release);
                starry_core::vsched::process_drop(old_pid);
                axlog::ax_println!("[execve] vsched pid {} -> {}", old_pid, new_pid);
                if root.as_usize() != 0 && root != kernel_root {
                    unsafe {
                        axhal::asm::write_user_page_table(kernel_root);
                        core::arch::asm!("sfence.vma");
                    }
                }
            }
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
// axlog::ax_println!("[execve] done, returning 0");
            uctx.set_sp(user_stack_base.as_usize());
            Ok(0)
        }
        Err(e) => {
// axlog::ax_println!("[execve] load_user_app FAILED: {:?}", e);
            drop(aspace);
            Err(e)
        }
    }
}
