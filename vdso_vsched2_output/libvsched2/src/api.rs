extern crate vsched2;
use alloc::vec::Vec;
pub use page_table_entry::MappingFlags;
pub use self::vsched2::*;

pub struct VdsoVTable {
    pub kernel_init_main: Option<fn(init_stack: *mut (), init_task_ptr: *const ())>,
    pub kernel_init_secondary: Option<fn(init_stack: *mut (), init_task_ptr: *const ())>,
    pub process_init: Option<fn(vspace_ptr: *mut *mut ()) -> usize>,
    pub process_drop: Option<fn(pid: usize)>,
    pub process_reinit: Option<fn(vspace_ptr: *mut *mut (), pid: usize)>,
    pub user_init: Option<fn()>,
    pub user_init_with_vspace: Option<fn(vspace: *mut ())>,
    pub user_scheduler_addr: Option<fn() -> *const ()>,
    pub push_task_into_current: Option<fn(task: *const ()) -> bool>,
    pub push_task: Option<fn(task: *const ()) -> bool>,
    pub push_task_into_process: Option<fn(task: *const (), pid: usize) -> bool>,
    pub current_vspace: Option<fn() -> usize>,
    pub trap_handler: Option<fn(queue: *const ())>,
    pub current_task_ptr: Option<fn() -> *const ()>,
    pub set_current_task_ptr: Option<fn(task: *const ()) -> *const ()>,
    pub take_current_stack: Option<fn() -> *mut ()>,
    pub raw_trap_entry: Option<fn() -> !>,
    pub raw_thread_entry: Option<fn() -> !>,
    pub raw_run_task: Option<fn() -> !>,
    pub raw_kschedule: Option<fn() -> !>,
    pub init_log: Option<fn(logger_fat_ptr: u128)>,
    pub init_vtable_Task: Option<fn(usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize)>,
    pub init_vtable_Stack: Option<fn(usize, usize, usize, usize)>,
    pub init_vtable_Context: Option<fn(usize, usize, usize)>,
    pub init_vtable_TrapInfo: Option<fn(usize, usize, usize, usize)>,
    pub init_vtable_SMP: Option<fn(usize)>,
    pub init_vtable_VSpace: Option<fn(usize)>,
    pub init_vtable_UserData: Option<fn(usize)>,
}

pub static mut VDSO_VTABLE: VdsoVTable = VdsoVTable {
    kernel_init_main: None,
    kernel_init_secondary: None,
    process_init: None,
    process_drop: None,
    process_reinit: None,
    user_init: None,
    user_init_with_vspace: None,
    user_scheduler_addr: None,
    push_task_into_current: None,
    push_task: None,
    push_task_into_process: None,
    current_vspace: None,
    trap_handler: None,
    current_task_ptr: None,
    set_current_task_ptr: None,
    take_current_stack: None,
    raw_trap_entry: None,
    raw_thread_entry: None,
    raw_run_task: None,
    raw_kschedule: None,
    init_log: None,
    init_vtable_Task: None,
    init_vtable_Stack: None,
    init_vtable_Context: None,
    init_vtable_TrapInfo: None,
    init_vtable_SMP: None,
    init_vtable_VSpace: None,
    init_vtable_UserData: None,
};

/// 在自身不加载vDSO，而是已经映射了vDSO的地址空间（通常是用户进程）中调用，传入vDSO的首地址以初始化VTABLE。
/// 
/// 在调用该库的其余API前，需先调用此函数。
pub unsafe fn init_vdso_vtable(base: u64) {
    // kernel_init_main:
    let fn_ptr = base + 0x4168;
    #[cfg(feature = "log")]
    log::debug!("kernel_init_main: 0x{:x}", fn_ptr);
    let f: fn(init_stack: *mut (), init_task_ptr: *const ()) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.kernel_init_main  = Some(f); }

    // kernel_init_secondary:
    let fn_ptr = base + 0x4334;
    #[cfg(feature = "log")]
    log::debug!("kernel_init_secondary: 0x{:x}", fn_ptr);
    let f: fn(init_stack: *mut (), init_task_ptr: *const ()) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.kernel_init_secondary  = Some(f); }

    // process_init:
    let fn_ptr = base + 0x45d2;
    #[cfg(feature = "log")]
    log::debug!("process_init: 0x{:x}", fn_ptr);
    let f: fn(vspace_ptr: *mut *mut ()) -> usize = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.process_init  = Some(f); }

    // process_drop:
    let fn_ptr = base + 0x4572;
    #[cfg(feature = "log")]
    log::debug!("process_drop: 0x{:x}", fn_ptr);
    let f: fn(pid: usize) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.process_drop  = Some(f); }

    // process_reinit:
    let fn_ptr = base + 0x485c;
    #[cfg(feature = "log")]
    log::debug!("process_reinit: 0x{:x}", fn_ptr);
    let f: fn(vspace_ptr: *mut *mut (), pid: usize) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.process_reinit  = Some(f); }

    // user_init:
    let fn_ptr = base + 0x52fa;
    #[cfg(feature = "log")]
    log::debug!("user_init: 0x{:x}", fn_ptr);
    let f: fn() = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.user_init  = Some(f); }

    // user_init_with_vspace:
    let fn_ptr = base + 0x5318;
    #[cfg(feature = "log")]
    log::debug!("user_init_with_vspace: 0x{:x}", fn_ptr);
    let f: fn(vspace: *mut ()) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.user_init_with_vspace  = Some(f); }

    // user_scheduler_addr:
    let fn_ptr = base + 0x5388;
    #[cfg(feature = "log")]
    log::debug!("user_scheduler_addr: 0x{:x}", fn_ptr);
    let f: fn() -> *const () = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.user_scheduler_addr  = Some(f); }

    // push_task_into_current:
    let fn_ptr = base + 0x4b9a;
    #[cfg(feature = "log")]
    log::debug!("push_task_into_current: 0x{:x}", fn_ptr);
    let f: fn(task: *const ()) -> bool = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.push_task_into_current  = Some(f); }

    // push_task:
    let fn_ptr = base + 0x4a98;
    #[cfg(feature = "log")]
    log::debug!("push_task: 0x{:x}", fn_ptr);
    let f: fn(task: *const ()) -> bool = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.push_task  = Some(f); }

    // push_task_into_process:
    let fn_ptr = base + 0x4bea;
    #[cfg(feature = "log")]
    log::debug!("push_task_into_process: 0x{:x}", fn_ptr);
    let f: fn(task: *const (), pid: usize) -> bool = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.push_task_into_process  = Some(f); }

    // current_vspace:
    let fn_ptr = base + 0x4100;
    #[cfg(feature = "log")]
    log::debug!("current_vspace: 0x{:x}", fn_ptr);
    let f: fn() -> usize = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.current_vspace  = Some(f); }

    // trap_handler:
    let fn_ptr = base + 0x4ec6;
    #[cfg(feature = "log")]
    log::debug!("trap_handler: 0x{:x}", fn_ptr);
    let f: fn(queue: *const ()) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.trap_handler  = Some(f); }

    // current_task_ptr:
    let fn_ptr = base + 0x4098;
    #[cfg(feature = "log")]
    log::debug!("current_task_ptr: 0x{:x}", fn_ptr);
    let f: fn() -> *const () = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.current_task_ptr  = Some(f); }

    // set_current_task_ptr:
    let fn_ptr = base + 0x4c76;
    #[cfg(feature = "log")]
    log::debug!("set_current_task_ptr: 0x{:x}", fn_ptr);
    let f: fn(task: *const ()) -> *const () = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.set_current_task_ptr  = Some(f); }

    // take_current_stack:
    let fn_ptr = base + 0x4ce2;
    #[cfg(feature = "log")]
    log::debug!("take_current_stack: 0x{:x}", fn_ptr);
    let f: fn() -> *mut () = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.take_current_stack  = Some(f); }

    // raw_trap_entry:
    let fn_ptr = base + 0x8f4;
    #[cfg(feature = "log")]
    log::debug!("raw_trap_entry: 0x{:x}", fn_ptr);
    let f: fn() -> ! = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.raw_trap_entry  = Some(f); }

    // raw_thread_entry:
    let fn_ptr = base + 0x924;
    #[cfg(feature = "log")]
    log::debug!("raw_thread_entry: 0x{:x}", fn_ptr);
    let f: fn() -> ! = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.raw_thread_entry  = Some(f); }

    // raw_run_task:
    let fn_ptr = base + 0x98e;
    #[cfg(feature = "log")]
    log::debug!("raw_run_task: 0x{:x}", fn_ptr);
    let f: fn() -> ! = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.raw_run_task  = Some(f); }

    // raw_kschedule:
    let fn_ptr = base + 0x944;
    #[cfg(feature = "log")]
    log::debug!("raw_kschedule: 0x{:x}", fn_ptr);
    let f: fn() -> ! = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.raw_kschedule  = Some(f); }

    // init_log:
    let fn_ptr = base + 0x5a22;
    #[cfg(feature = "log")]
    log::debug!("init_log: 0x{:x}", fn_ptr);
    let f: fn(logger_fat_ptr: u128) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.init_log  = Some(f); }

    // init_vtable_Task:
    let fn_ptr = base + 0x57a4;
    #[cfg(feature = "log")]
    log::debug!("init_vtable_Task: 0x{:x}", fn_ptr);
    let f: fn(usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.init_vtable_Task  = Some(f); }

    // init_vtable_Stack:
    let fn_ptr = base + 0x5770;
    #[cfg(feature = "log")]
    log::debug!("init_vtable_Stack: 0x{:x}", fn_ptr);
    let f: fn(usize, usize, usize, usize) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.init_vtable_Stack  = Some(f); }

    // init_vtable_Context:
    let fn_ptr = base + 0x571c;
    #[cfg(feature = "log")]
    log::debug!("init_vtable_Context: 0x{:x}", fn_ptr);
    let f: fn(usize, usize, usize) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.init_vtable_Context  = Some(f); }

    // init_vtable_TrapInfo:
    let fn_ptr = base + 0x5810;
    #[cfg(feature = "log")]
    log::debug!("init_vtable_TrapInfo: 0x{:x}", fn_ptr);
    let f: fn(usize, usize, usize, usize) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.init_vtable_TrapInfo  = Some(f); }

    // init_vtable_SMP:
    let fn_ptr = base + 0x574c;
    #[cfg(feature = "log")]
    log::debug!("init_vtable_SMP: 0x{:x}", fn_ptr);
    let f: fn(usize) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.init_vtable_SMP  = Some(f); }

    // init_vtable_VSpace:
    let fn_ptr = base + 0x5868;
    #[cfg(feature = "log")]
    log::debug!("init_vtable_VSpace: 0x{:x}", fn_ptr);
    let f: fn(usize) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.init_vtable_VSpace  = Some(f); }

    // init_vtable_UserData:
    let fn_ptr = base + 0x5844;
    #[cfg(feature = "log")]
    log::debug!("init_vtable_UserData: 0x{:x}", fn_ptr);
    let f: fn(usize) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.init_vtable_UserData  = Some(f); }

}
    
/// 在加载vDSO的地址空间（通常是内核）中调用，同时加载vDSO和初始化VTABLE。
/// 
/// 若在一个地址空间中加载再映射到另一个地址空间中，需使用`map_and_init`。
/// 
/// 该函数的返回值为vDSO和vVAR的映射区域的信息，元组的三项依次为首地址、大小和访问权限。vDSO首地址为第二个映射区域的首地址。
/// 
/// 在调用该库的其余API前，需先调用此函数。
pub fn load_and_init(vspace: usize) {
    let vdso = crate::map_so(vspace);
    unsafe{ init_vdso_vtable(vdso as _) };
    init_vdso_log();
}

fn init_vdso_log() {
    let logger = log::logger();
    let fat_ptr: u128 = unsafe { core::mem::transmute(logger) };
    init_log(fat_ptr);
}


pub fn kernel_init_main(init_stack: *mut (), init_task_ptr: *const ()) {
    if let Some(f) = unsafe { VDSO_VTABLE.kernel_init_main } {
        #[cfg(feature = "log")]
        log::debug!("Calling kernel_init_main at 0x{:x}.", f as *const () as usize);
        let res = f(init_stack, init_task_ptr);
        #[cfg(feature = "log")]
        log::debug!("Returned from kernel_init_main.");
        res
    } else {
        panic!("kernel_init_main is not initialized")
    }
}

pub fn kernel_init_secondary(init_stack: *mut (), init_task_ptr: *const ()) {
    if let Some(f) = unsafe { VDSO_VTABLE.kernel_init_secondary } {
        #[cfg(feature = "log")]
        log::debug!("Calling kernel_init_secondary at 0x{:x}.", f as *const () as usize);
        let res = f(init_stack, init_task_ptr);
        #[cfg(feature = "log")]
        log::debug!("Returned from kernel_init_secondary.");
        res
    } else {
        panic!("kernel_init_secondary is not initialized")
    }
}

pub fn process_init(vspace_ptr: *mut *mut ()) -> usize {
    if let Some(f) = unsafe { VDSO_VTABLE.process_init } {
        #[cfg(feature = "log")]
        log::debug!("Calling process_init at 0x{:x}.", f as *const () as usize);
        let res = f(vspace_ptr);
        #[cfg(feature = "log")]
        log::debug!("Returned from process_init.");
        res
    } else {
        panic!("process_init is not initialized")
    }
}

pub fn process_drop(pid: usize) {
    if let Some(f) = unsafe { VDSO_VTABLE.process_drop } {
        #[cfg(feature = "log")]
        log::debug!("Calling process_drop at 0x{:x}.", f as *const () as usize);
        let res = f(pid);
        #[cfg(feature = "log")]
        log::debug!("Returned from process_drop.");
        res
    } else {
        panic!("process_drop is not initialized")
    }
}

pub fn process_reinit(vspace_ptr: *mut *mut (), pid: usize) {
    if let Some(f) = unsafe { VDSO_VTABLE.process_reinit } {
        #[cfg(feature = "log")]
        log::debug!("Calling process_reinit at 0x{:x}.", f as *const () as usize);
        let res = f(vspace_ptr, pid);
        #[cfg(feature = "log")]
        log::debug!("Returned from process_reinit.");
        res
    } else {
        panic!("process_reinit is not initialized")
    }
}

pub fn user_init() {
    if let Some(f) = unsafe { VDSO_VTABLE.user_init } {
        #[cfg(feature = "log")]
        log::debug!("Calling user_init at 0x{:x}.", f as *const () as usize);
        let res = f();
        #[cfg(feature = "log")]
        log::debug!("Returned from user_init.");
        res
    } else {
        panic!("user_init is not initialized")
    }
}

pub fn user_init_with_vspace(vspace: *mut ()) {
    if let Some(f) = unsafe { VDSO_VTABLE.user_init_with_vspace } {
        #[cfg(feature = "log")]
        log::debug!("Calling user_init_with_vspace at 0x{:x}.", f as *const () as usize);
        let res = f(vspace);
        #[cfg(feature = "log")]
        log::debug!("Returned from user_init_with_vspace.");
        res
    } else {
        panic!("user_init_with_vspace is not initialized")
    }
}

pub fn user_scheduler_addr() -> *const () {
    if let Some(f) = unsafe { VDSO_VTABLE.user_scheduler_addr } {
        #[cfg(feature = "log")]
        log::debug!("Calling user_scheduler_addr at 0x{:x}.", f as *const () as usize);
        let res = f();
        #[cfg(feature = "log")]
        log::debug!("Returned from user_scheduler_addr.");
        res
    } else {
        panic!("user_scheduler_addr is not initialized")
    }
}

pub fn push_task_into_current(task: *const ()) -> bool {
    if let Some(f) = unsafe { VDSO_VTABLE.push_task_into_current } {
        #[cfg(feature = "log")]
        log::debug!("Calling push_task_into_current at 0x{:x}.", f as *const () as usize);
        let res = f(task);
        #[cfg(feature = "log")]
        log::debug!("Returned from push_task_into_current.");
        res
    } else {
        panic!("push_task_into_current is not initialized")
    }
}

pub fn push_task(task: *const ()) -> bool {
    if let Some(f) = unsafe { VDSO_VTABLE.push_task } {
        #[cfg(feature = "log")]
        log::debug!("Calling push_task at 0x{:x}.", f as *const () as usize);
        let res = f(task);
        #[cfg(feature = "log")]
        log::debug!("Returned from push_task.");
        res
    } else {
        panic!("push_task is not initialized")
    }
}

pub fn push_task_into_process(task: *const (), pid: usize) -> bool {
    if let Some(f) = unsafe { VDSO_VTABLE.push_task_into_process } {
        #[cfg(feature = "log")]
        log::debug!("Calling push_task_into_process at 0x{:x}.", f as *const () as usize);
        let res = f(task, pid);
        #[cfg(feature = "log")]
        log::debug!("Returned from push_task_into_process.");
        res
    } else {
        panic!("push_task_into_process is not initialized")
    }
}

pub fn current_vspace() -> usize {
    if let Some(f) = unsafe { VDSO_VTABLE.current_vspace } {
        #[cfg(feature = "log")]
        log::debug!("Calling current_vspace at 0x{:x}.", f as *const () as usize);
        let res = f();
        #[cfg(feature = "log")]
        log::debug!("Returned from current_vspace.");
        res
    } else {
        panic!("current_vspace is not initialized")
    }
}

pub fn trap_handler(queue: *const ()) {
    if let Some(f) = unsafe { VDSO_VTABLE.trap_handler } {
        #[cfg(feature = "log")]
        log::debug!("Calling trap_handler at 0x{:x}.", f as *const () as usize);
        let res = f(queue);
        #[cfg(feature = "log")]
        log::debug!("Returned from trap_handler.");
        res
    } else {
        panic!("trap_handler is not initialized")
    }
}

pub fn current_task_ptr() -> *const () {
    if let Some(f) = unsafe { VDSO_VTABLE.current_task_ptr } {
        #[cfg(feature = "log")]
        log::debug!("Calling current_task_ptr at 0x{:x}.", f as *const () as usize);
        let res = f();
        #[cfg(feature = "log")]
        log::debug!("Returned from current_task_ptr.");
        res
    } else {
        panic!("current_task_ptr is not initialized")
    }
}

pub fn set_current_task_ptr(task: *const ()) -> *const () {
    if let Some(f) = unsafe { VDSO_VTABLE.set_current_task_ptr } {
        #[cfg(feature = "log")]
        log::debug!("Calling set_current_task_ptr at 0x{:x}.", f as *const () as usize);
        let res = f(task);
        #[cfg(feature = "log")]
        log::debug!("Returned from set_current_task_ptr.");
        res
    } else {
        panic!("set_current_task_ptr is not initialized")
    }
}

pub fn take_current_stack() -> *mut () {
    if let Some(f) = unsafe { VDSO_VTABLE.take_current_stack } {
        #[cfg(feature = "log")]
        log::debug!("Calling take_current_stack at 0x{:x}.", f as *const () as usize);
        let res = f();
        #[cfg(feature = "log")]
        log::debug!("Returned from take_current_stack.");
        res
    } else {
        panic!("take_current_stack is not initialized")
    }
}

pub fn raw_trap_entry() -> ! {
    if let Some(f) = unsafe { VDSO_VTABLE.raw_trap_entry } {
        #[cfg(feature = "log")]
        log::debug!("Calling raw_trap_entry at 0x{:x}.", f as *const () as usize);
        let res = f();
        #[cfg(feature = "log")]
        log::debug!("Returned from raw_trap_entry.");
        res
    } else {
        panic!("raw_trap_entry is not initialized")
    }
}

pub fn raw_thread_entry() -> ! {
    if let Some(f) = unsafe { VDSO_VTABLE.raw_thread_entry } {
        #[cfg(feature = "log")]
        log::debug!("Calling raw_thread_entry at 0x{:x}.", f as *const () as usize);
        let res = f();
        #[cfg(feature = "log")]
        log::debug!("Returned from raw_thread_entry.");
        res
    } else {
        panic!("raw_thread_entry is not initialized")
    }
}

pub fn raw_run_task() -> ! {
    if let Some(f) = unsafe { VDSO_VTABLE.raw_run_task } {
        #[cfg(feature = "log")]
        log::debug!("Calling raw_run_task at 0x{:x}.", f as *const () as usize);
        let res = f();
        #[cfg(feature = "log")]
        log::debug!("Returned from raw_run_task.");
        res
    } else {
        panic!("raw_run_task is not initialized")
    }
}

pub fn raw_kschedule() -> ! {
    if let Some(f) = unsafe { VDSO_VTABLE.raw_kschedule } {
        #[cfg(feature = "log")]
        log::debug!("Calling raw_kschedule at 0x{:x}.", f as *const () as usize);
        let res = f();
        #[cfg(feature = "log")]
        log::debug!("Returned from raw_kschedule.");
        res
    } else {
        panic!("raw_kschedule is not initialized")
    }
}

pub fn init_log(logger_fat_ptr: u128) {
    if let Some(f) = unsafe { VDSO_VTABLE.init_log } {
        #[cfg(feature = "log")]
        log::debug!("Calling init_log at 0x{:x}.", f as *const () as usize);
        let res = f(logger_fat_ptr);
        #[cfg(feature = "log")]
        log::debug!("Returned from init_log.");
        res
    } else {
        panic!("init_log is not initialized")
    }
}

pub fn init_vtable_Task<T:Task>() {
    if let Some(f) = unsafe { VDSO_VTABLE.init_vtable_Task } {
        #[cfg(feature = "log")]
        log::debug!("Calling init_vtable_Task at 0x{:x}.", f as *const () as usize);
        let res = f(T::state as usize, T::set_state as usize, T::priority as usize, T::is_coroutine as usize, T::is_kernel as usize, T::pid as usize, T::set_pid as usize, T::resched as usize, T::restore_context as usize, T::poll as usize, T::thread_stack as usize, T::set_return_value as usize, T::dealloc as usize);
        #[cfg(feature = "log")]
        log::debug!("Returned from init_vtable_Task.");
        res
    } else {
        panic!("init_vtable_Task is not initialized")
    }
}

pub fn init_vtable_Stack<T:Stack>() {
    if let Some(f) = unsafe { VDSO_VTABLE.init_vtable_Stack } {
        #[cfg(feature = "log")]
        log::debug!("Calling init_vtable_Stack at 0x{:x}.", f as *const () as usize);
        let res = f(T::alloc as usize, T::dealloc as usize, T::base as usize, T::from_base as usize);
        #[cfg(feature = "log")]
        log::debug!("Returned from init_vtable_Stack.");
        res
    } else {
        panic!("init_vtable_Stack is not initialized")
    }
}

pub fn init_vtable_Context<T:Context>() {
    if let Some(f) = unsafe { VDSO_VTABLE.init_vtable_Context } {
        #[cfg(feature = "log")]
        log::debug!("Calling init_vtable_Context at 0x{:x}.", f as *const () as usize);
        let res = f(T::into_kernel as usize, T::into_user as usize, T::into_user_context as usize);
        #[cfg(feature = "log")]
        log::debug!("Returned from init_vtable_Context.");
        res
    } else {
        panic!("init_vtable_Context is not initialized")
    }
}

pub fn init_vtable_TrapInfo<T:TrapInfo>() {
    if let Some(f) = unsafe { VDSO_VTABLE.init_vtable_TrapInfo } {
        #[cfg(feature = "log")]
        log::debug!("Calling init_vtable_TrapInfo at 0x{:x}.", f as *const () as usize);
        let res = f(T::from_task as usize, T::handle as usize, T::dealloc as usize, T::new_handler as usize);
        #[cfg(feature = "log")]
        log::debug!("Returned from init_vtable_TrapInfo.");
        res
    } else {
        panic!("init_vtable_TrapInfo is not initialized")
    }
}

pub fn init_vtable_SMP<T:SMP>() {
    if let Some(f) = unsafe { VDSO_VTABLE.init_vtable_SMP } {
        #[cfg(feature = "log")]
        log::debug!("Calling init_vtable_SMP at 0x{:x}.", f as *const () as usize);
        let res = f(T::cpu_id as usize);
        #[cfg(feature = "log")]
        log::debug!("Returned from init_vtable_SMP.");
        res
    } else {
        panic!("init_vtable_SMP is not initialized")
    }
}

pub fn init_vtable_VSpace<T:VSpace>() {
    if let Some(f) = unsafe { VDSO_VTABLE.init_vtable_VSpace } {
        #[cfg(feature = "log")]
        log::debug!("Calling init_vtable_VSpace at 0x{:x}.", f as *const () as usize);
        let res = f(T::into_vspace as usize);
        #[cfg(feature = "log")]
        log::debug!("Returned from init_vtable_VSpace.");
        res
    } else {
        panic!("init_vtable_VSpace is not initialized")
    }
}

pub fn init_vtable_UserData<T:UserData>() {
    if let Some(f) = unsafe { VDSO_VTABLE.init_vtable_UserData } {
        #[cfg(feature = "log")]
        log::debug!("Calling init_vtable_UserData at 0x{:x}.", f as *const () as usize);
        let res = f(T::get_user_data as usize);
        #[cfg(feature = "log")]
        log::debug!("Returned from init_vtable_UserData.");
        res
    } else {
        panic!("init_vtable_UserData is not initialized")
    }
}
