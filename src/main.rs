#![no_std]
#![no_main]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate axlog;

extern crate alloc;
extern crate axruntime;

mod entry;

use alloc::{
    borrow::ToOwned,
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::sync::atomic::Ordering;
use axfs::FS_CONTEXT;
use axhal::uspace::UserContext;
use axsync::Mutex;
use axtask::AxTaskExt;
use starry_api::{file::FD_TABLE, task::new_user_task, vfs::dev::tty::N_TTY};
#[allow(unused_imports)]
use starry_core::{
    mm::{copy_from_kernel, load_user_app, new_user_aspace_empty},
    task::{ProcessData, Thread, add_task_to_table},
    vsched::VschedTaskImpl,
};
use starry_core::vsched::trapframe::{UserTrapFrame, UserTrapFrameKind};
use starry_process::{Pid, Process};

const USE_VSCHED2: bool = false;

pub const CMDLINE: &[&str] = &["/bin/sh", "-c", include_str!("init.sh")];

fn create_vsched_init_task(args: &[String], envs: &[String]) -> (*const starry_core::vsched::VschedTaskImpl, *mut *mut ()) {
    let mut uspace = new_user_aspace_empty()
        .and_then(|mut it| {
            copy_from_kernel(&mut it)?;
            Ok(it)
        })
        .expect("Failed to create user address space");

    let loc = FS_CONTEXT
        .lock()
        .resolve(&args[0])
        .expect("Failed to resolve executable path");
    let path = loc
        .absolute_path()
        .expect("Failed to get executable absolute path");
    let name = loc.name();

    let (entry_vaddr, ustack_top) = load_user_app(&mut uspace, None, args, envs)
        .unwrap_or_else(|e| panic!("Failed to load user app: {}", e));
    axlog::ax_println!("create_vsched_init_task: entry={:#x}, ustack_top={:#x}", entry_vaddr.as_usize(), ustack_top.as_usize());

    let uctx = UserContext::new(entry_vaddr.into(), ustack_top, 0);

    let mut task = new_user_task(name, uctx, 0);
    task.ctx_mut().set_page_table_root(uspace.page_table_root());

    let pid = task.id().as_u64() as Pid;
    let proc = Process::new_init(pid);
    proc.add_thread(pid);
    N_TTY.bind_to(&proc).expect("Failed to bind ntty");

    let proc_data = ProcessData::new(
        proc,
        path.to_string(),
        Arc::new(args.to_vec()),
        Arc::new(Mutex::new(uspace)),
        Arc::default(),
        None,
    );
    // 获取 AddrSpace 的稳定指针（AddrSpace 存储在 Mutex 内部，Mutex 在 Arc 中，生命周期安全）
    let vspace_ptr = {
        let guard = proc_data.aspace.lock();
        let p: *mut () = &raw const *guard as *mut ();
        Box::into_raw(Box::new(p))
    };
    // 保存用户页表根，稍后存入 VschedTaskImpl
    let user_root = proc_data.aspace.lock().page_table_root();
    // 保存 Mutex 裸指针用于后续 copy_mappings_from
    let aspace_mutex_ptr = Arc::as_ptr(&proc_data.aspace) as usize;
    let thr = Thread::new(pid, proc_data);
    *task.task_ext_mut() = Some(unsafe { AxTaskExt::from_impl(thr) });

    let entry_ra = task.ctx_mut().ra;
    let entry_sp = task.ctx_mut().sp;

    let task_ref = axtask::into_ref(task);
    add_task_to_table(&task_ref);

    // Add stdio while user task is current
    use starry_core::task::AsThread;
    axtask::with_current_task(&task_ref, || {
        let thr = task_ref.try_as_thread().expect("user task should have thread");
        let mut scope = thr.proc_data.scope.write();
        starry_api::file::add_stdio(&mut FD_TABLE.scope_mut(&mut scope).write())
            .expect("Failed to add stdio");
    });

    let mut tf = Box::new(UserTrapFrame {
        regs: unsafe { core::mem::zeroed() },
        sepc: uctx.sepc,
        sstatus: uctx.sstatus.bits(),
        scause: 0,
        stval: 0,
        kind: UserTrapFrameKind::Yield,
    });
    // 从 UserContext 拷贝关键寄存器
    tf.regs.a0 = uctx.regs.a0;
    tf.regs.sp = uctx.regs.sp;
    // User entry point: ra=0 so a stray 'ret' crashes cleanly instead
    // of jumping to a kernel address.
    tf.regs.ra = 0;
    let tf_ptr = Box::into_raw(tf);

    let vti = starry_core::vsched::register_task(task_ref.clone(), 0, 1, None);
    // 为 init 任务分配一个 Stack 对象
    let init_stack_ptr = starry_core::vsched::alloc_stack();
    unsafe { &*vti }.thread_stack_ptr.store(init_stack_ptr as usize, Ordering::Release);
    unsafe { &*vti }.trap_frame.store(tf_ptr as usize, Ordering::Release);
    unsafe { &*vti }.user_page_table_root.store(user_root.as_usize(), Ordering::Release);
    unsafe { &*vti }.user_aspace_ptr.store(aspace_mutex_ptr, Ordering::Release);
    (vti, vspace_ptr)
}

#[unsafe(no_mangle)]
fn main() {
    starry_api::init();
    vdso::vdso_init();

    let args = CMDLINE
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let envs: &[String] = &[];
    // axlog::ax_println!("main: USE_VSCHED2={} args={:?}", USE_VSCHED2, args);

    if USE_VSCHED2 {
        let (init_ptr, vspace_ptr) = create_vsched_init_task(&args, envs);
        starry_core::vsched::vsched2_bootstrap(Some(init_ptr as *const ()), Some(vspace_ptr));
    } else {
        entry::run_initproc(&args, envs);
    }

    // SBI shutdown (SRST extension)
    unsafe {
        core::arch::asm!(
            "li a7, 0x53525354",
            "li a6, 0",
            "li a0, 0",
            "li a1, 0",
            "ecall",
            options(noreturn),
        );
    }
}
