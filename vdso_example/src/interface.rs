vdso_helper::trait_interface! {
    pub trait TestIf {
        fn test_fn1(&self, arg: usize) -> usize;
        fn test_fn2(&mut self, arg: usize) -> usize;
        fn test_fn3(arg: usize);
    }
}

pub fn test_call(ptr: *mut ()) {
    let virt = unsafe { TestIfVirtImpl::from_mut(ptr) };
    virt.test_fn1(1);
    virt.test_fn2(2);
    TestIfVirtImpl::test_fn3(3);
}
