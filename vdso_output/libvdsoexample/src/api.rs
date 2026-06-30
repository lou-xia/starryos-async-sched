extern crate vdso_example;
use alloc::vec::Vec;
pub use page_table_entry::MappingFlags;
pub use self::vdso_example::*;

struct VdsoVTable {
    pub get_shared: Option<fn() -> ArgumentExample>,
    pub set_shared: Option<fn(i: usize)>,
    pub get_private: Option<fn() -> ArgumentExample>,
    pub set_private: Option<fn(i: usize)>,
    pub test_args: Option<fn(a: Option<usize>,b: Result<usize, ()>,c: (usize, usize),) -> (Option<usize>, Result<usize, ()>, (usize, usize))>,
    pub test_call: Option<fn(ptr: *mut ())>,
    pub init_vtable_TestIf: Option<fn(usize, usize, usize)>,
}

static mut VDSO_VTABLE: VdsoVTable = VdsoVTable {
    get_shared: None,
    set_shared: None,
    get_private: None,
    set_private: None,
    test_args: None,
    test_call: None,
    init_vtable_TestIf: None,
};

/// 在自身不加载vDSO，而是已经映射了vDSO的地址空间（通常是用户进程）中调用，传入vDSO的首地址以初始化VTABLE。
/// 
/// 在调用该库的其余API前，需先调用此函数。
pub unsafe fn init_vdso_vtable(base: u64) {
    // get_shared:
    let fn_ptr = base + 0x11d2;
    #[cfg(feature = "log")]
    log::debug!("get_shared: 0x{:x}", fn_ptr);
    let f: fn() -> ArgumentExample = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.get_shared  = Some(f); }

    // set_shared:
    let fn_ptr = base + 0x12da;
    #[cfg(feature = "log")]
    log::debug!("set_shared: 0x{:x}", fn_ptr);
    let f: fn(i: usize) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.set_shared  = Some(f); }

    // get_private:
    let fn_ptr = base + 0x11b6;
    #[cfg(feature = "log")]
    log::debug!("get_private: 0x{:x}", fn_ptr);
    let f: fn() -> ArgumentExample = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.get_private  = Some(f); }

    // set_private:
    let fn_ptr = base + 0x12be;
    #[cfg(feature = "log")]
    log::debug!("set_private: 0x{:x}", fn_ptr);
    let f: fn(i: usize) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.set_private  = Some(f); }

    // test_args:
    let fn_ptr = base + 0x1306;
    #[cfg(feature = "log")]
    log::debug!("test_args: 0x{:x}", fn_ptr);
    let f: fn(a: Option<usize>,b: Result<usize, ()>,c: (usize, usize),) -> (Option<usize>, Result<usize, ()>, (usize, usize)) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.test_args  = Some(f); }

    // test_call:
    let fn_ptr = base + 0x1330;
    #[cfg(feature = "log")]
    log::debug!("test_call: 0x{:x}", fn_ptr);
    let f: fn(ptr: *mut ()) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.test_call  = Some(f); }

    // init_vtable_TestIf:
    let fn_ptr = base + 0x11f8;
    #[cfg(feature = "log")]
    log::debug!("init_vtable_TestIf: 0x{:x}", fn_ptr);
    let f: fn(usize, usize, usize) = unsafe { core::mem::transmute(fn_ptr) };
    unsafe { VDSO_VTABLE.init_vtable_TestIf  = Some(f); }

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
}

pub fn get_shared() -> ArgumentExample {
    if let Some(f) = unsafe { VDSO_VTABLE.get_shared } {
        #[cfg(feature = "log")]
        log::debug!("Calling get_shared at 0x{:x}.", f as *const () as usize);
        f()
    } else {
        panic!("get_shared is not initialized")
    }
}

pub fn set_shared(i: usize) {
    if let Some(f) = unsafe { VDSO_VTABLE.set_shared } {
        #[cfg(feature = "log")]
        log::debug!("Calling set_shared at 0x{:x}.", f as *const () as usize);
        f(i)
    } else {
        panic!("set_shared is not initialized")
    }
}

pub fn get_private() -> ArgumentExample {
    if let Some(f) = unsafe { VDSO_VTABLE.get_private } {
        #[cfg(feature = "log")]
        log::debug!("Calling get_private at 0x{:x}.", f as *const () as usize);
        f()
    } else {
        panic!("get_private is not initialized")
    }
}

pub fn set_private(i: usize) {
    if let Some(f) = unsafe { VDSO_VTABLE.set_private } {
        #[cfg(feature = "log")]
        log::debug!("Calling set_private at 0x{:x}.", f as *const () as usize);
        f(i)
    } else {
        panic!("set_private is not initialized")
    }
}

pub fn test_args(a: Option<usize>,b: Result<usize, ()>,c: (usize, usize),) -> (Option<usize>, Result<usize, ()>, (usize, usize)) {
    if let Some(f) = unsafe { VDSO_VTABLE.test_args } {
        #[cfg(feature = "log")]
        log::debug!("Calling test_args at 0x{:x}.", f as *const () as usize);
        f(a, b, c)
    } else {
        panic!("test_args is not initialized")
    }
}

pub fn test_call(ptr: *mut ()) {
    if let Some(f) = unsafe { VDSO_VTABLE.test_call } {
        #[cfg(feature = "log")]
        log::debug!("Calling test_call at 0x{:x}.", f as *const () as usize);
        f(ptr)
    } else {
        panic!("test_call is not initialized")
    }
}

pub fn init_vtable_TestIf<T:TestIf>() {
    if let Some(f) = unsafe { VDSO_VTABLE.init_vtable_TestIf } {
        #[cfg(feature = "log")]
        log::debug!("Calling init_vtable_TestIf at 0x{:x}.", f as *const () as usize);
        f(T::test_fn1 as usize, T::test_fn2 as usize, T::test_fn3 as usize)
    } else {
        panic!("init_vtable_TestIf is not initialized")
    }
}
