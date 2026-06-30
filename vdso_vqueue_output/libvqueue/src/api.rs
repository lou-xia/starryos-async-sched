extern crate vqueue;
use alloc::vec::Vec;
pub use page_table_entry::MappingFlags;
pub use self::vqueue::*;

pub struct VdsoVTable {
    pub register_process: Option<fn() -> Result<SlotRef<'static, PerProcess, ARRAY_LEN>, ()>>,
    pub deque_push: Option<fn(process_id: usize, item: IPCItem) -> Result<(), IPCItem>>,
    pub deque_pop: Option<fn(process_id: usize) -> Option<IPCItem>>,
    pub slotref_from_id: Option<fn(process_id: usize) -> SlotRef<'static, PerProcess, ARRAY_LEN>>,
    pub get_pid: Option<fn(process_id: usize) -> usize>,
    pub set_pid: Option<fn(process_id: usize, pid: usize)>,
    pub map_add_entry: Option<fn(process_id: usize,msg_type: usize,ntf_id: usize,) -> Result<(), ()>>,
    pub map_get_ntf_id: Option<fn(process_id: usize, msg_type: usize) -> Option<usize>>,
    pub map_pop_ntf_id: Option<fn(process_id: usize, msg_type: usize) -> Option<usize>>,
}

pub static mut VDSO_VTABLE: VdsoVTable = VdsoVTable {
    register_process: None,
    deque_push: None,
    deque_pop: None,
    slotref_from_id: None,
    get_pid: None,
    set_pid: None,
    map_add_entry: None,
    map_get_ntf_id: None,
    map_pop_ntf_id: None,
};

/// 在自身不加载vDSO，而是已经映射了vDSO的地址空间（通常是用户进程）中调用，传入vDSO的首地址以初始化VTABLE。
/// 
/// 在调用该库的其余API前，需先调用此函数。
pub unsafe fn init_vdso_vtable(base: u64) {
    // register_process:
    let fn_ptr = base + 0x2140;
    #[cfg(feature = "log")]
    log::debug!("register_process: 0x{:x}", fn_ptr);
    let f: fn() -> Result<SlotRef<'static, PerProcess, ARRAY_LEN>, ()> = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.register_process  = Some(f); }

    // deque_push:
    let fn_ptr = base + 0x6fe;
    #[cfg(feature = "log")]
    log::debug!("deque_push: 0x{:x}", fn_ptr);
    let f: fn(process_id: usize, item: IPCItem) -> Result<(), IPCItem> = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.deque_push  = Some(f); }

    // deque_pop:
    let fn_ptr = base + 0x4ca;
    #[cfg(feature = "log")]
    log::debug!("deque_pop: 0x{:x}", fn_ptr);
    let f: fn(process_id: usize) -> Option<IPCItem> = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.deque_pop  = Some(f); }

    // slotref_from_id:
    let fn_ptr = base + 0x24fe;
    #[cfg(feature = "log")]
    log::debug!("slotref_from_id: 0x{:x}", fn_ptr);
    let f: fn(process_id: usize) -> SlotRef<'static, PerProcess, ARRAY_LEN> = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.slotref_from_id  = Some(f); }

    // get_pid:
    let fn_ptr = base + 0x934;
    #[cfg(feature = "log")]
    log::debug!("get_pid: 0x{:x}", fn_ptr);
    let f: fn(process_id: usize) -> usize = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.get_pid  = Some(f); }

    // set_pid:
    let fn_ptr = base + 0x2408;
    #[cfg(feature = "log")]
    log::debug!("set_pid: 0x{:x}", fn_ptr);
    let f: fn(process_id: usize, pid: usize) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.set_pid  = Some(f); }

    // map_add_entry:
    let fn_ptr = base + 0xa24;
    #[cfg(feature = "log")]
    log::debug!("map_add_entry: 0x{:x}", fn_ptr);
    let f: fn(process_id: usize,msg_type: usize,ntf_id: usize,) -> Result<(), ()> = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.map_add_entry  = Some(f); }

    // map_get_ntf_id:
    let fn_ptr = base + 0x1d8e;
    #[cfg(feature = "log")]
    log::debug!("map_get_ntf_id: 0x{:x}", fn_ptr);
    let f: fn(process_id: usize, msg_type: usize) -> Option<usize> = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.map_get_ntf_id  = Some(f); }

    // map_pop_ntf_id:
    let fn_ptr = base + 0x1ec6;
    #[cfg(feature = "log")]
    log::debug!("map_pop_ntf_id: 0x{:x}", fn_ptr);
    let f: fn(process_id: usize, msg_type: usize) -> Option<usize> = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.map_pop_ntf_id  = Some(f); }

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

fn init_vdso_log() {}


pub fn register_process() -> Result<SlotRef<'static, PerProcess, ARRAY_LEN>, ()> {
    if let Some(f) = unsafe { VDSO_VTABLE.register_process } {
        #[cfg(feature = "log")]
        log::debug!("Calling register_process at 0x{:x}.", f as *const () as usize);
        let res = f();
        #[cfg(feature = "log")]
        log::debug!("Returned from register_process.");
        res
    } else {
        panic!("register_process is not initialized")
    }
}

pub fn deque_push(process_id: usize, item: IPCItem) -> Result<(), IPCItem> {
    if let Some(f) = unsafe { VDSO_VTABLE.deque_push } {
        #[cfg(feature = "log")]
        log::debug!("Calling deque_push at 0x{:x}.", f as *const () as usize);
        let res = f(process_id, item);
        #[cfg(feature = "log")]
        log::debug!("Returned from deque_push.");
        res
    } else {
        panic!("deque_push is not initialized")
    }
}

pub fn deque_pop(process_id: usize) -> Option<IPCItem> {
    if let Some(f) = unsafe { VDSO_VTABLE.deque_pop } {
        #[cfg(feature = "log")]
        log::debug!("Calling deque_pop at 0x{:x}.", f as *const () as usize);
        let res = f(process_id);
        #[cfg(feature = "log")]
        log::debug!("Returned from deque_pop.");
        res
    } else {
        panic!("deque_pop is not initialized")
    }
}

pub fn slotref_from_id(process_id: usize) -> SlotRef<'static, PerProcess, ARRAY_LEN> {
    if let Some(f) = unsafe { VDSO_VTABLE.slotref_from_id } {
        #[cfg(feature = "log")]
        log::debug!("Calling slotref_from_id at 0x{:x}.", f as *const () as usize);
        let res = f(process_id);
        #[cfg(feature = "log")]
        log::debug!("Returned from slotref_from_id.");
        res
    } else {
        panic!("slotref_from_id is not initialized")
    }
}

pub fn get_pid(process_id: usize) -> usize {
    if let Some(f) = unsafe { VDSO_VTABLE.get_pid } {
        #[cfg(feature = "log")]
        log::debug!("Calling get_pid at 0x{:x}.", f as *const () as usize);
        let res = f(process_id);
        #[cfg(feature = "log")]
        log::debug!("Returned from get_pid.");
        res
    } else {
        panic!("get_pid is not initialized")
    }
}

pub fn set_pid(process_id: usize, pid: usize) {
    if let Some(f) = unsafe { VDSO_VTABLE.set_pid } {
        #[cfg(feature = "log")]
        log::debug!("Calling set_pid at 0x{:x}.", f as *const () as usize);
        let res = f(process_id, pid);
        #[cfg(feature = "log")]
        log::debug!("Returned from set_pid.");
        res
    } else {
        panic!("set_pid is not initialized")
    }
}

pub fn map_add_entry(process_id: usize,msg_type: usize,ntf_id: usize,) -> Result<(), ()> {
    if let Some(f) = unsafe { VDSO_VTABLE.map_add_entry } {
        #[cfg(feature = "log")]
        log::debug!("Calling map_add_entry at 0x{:x}.", f as *const () as usize);
        let res = f(process_id, msg_type, ntf_id);
        #[cfg(feature = "log")]
        log::debug!("Returned from map_add_entry.");
        res
    } else {
        panic!("map_add_entry is not initialized")
    }
}

pub fn map_get_ntf_id(process_id: usize, msg_type: usize) -> Option<usize> {
    if let Some(f) = unsafe { VDSO_VTABLE.map_get_ntf_id } {
        #[cfg(feature = "log")]
        log::debug!("Calling map_get_ntf_id at 0x{:x}.", f as *const () as usize);
        let res = f(process_id, msg_type);
        #[cfg(feature = "log")]
        log::debug!("Returned from map_get_ntf_id.");
        res
    } else {
        panic!("map_get_ntf_id is not initialized")
    }
}

pub fn map_pop_ntf_id(process_id: usize, msg_type: usize) -> Option<usize> {
    if let Some(f) = unsafe { VDSO_VTABLE.map_pop_ntf_id } {
        #[cfg(feature = "log")]
        log::debug!("Calling map_pop_ntf_id at 0x{:x}.", f as *const () as usize);
        let res = f(process_id, msg_type);
        #[cfg(feature = "log")]
        log::debug!("Returned from map_pop_ntf_id.");
        res
    } else {
        panic!("map_pop_ntf_id is not initialized")
    }
}
