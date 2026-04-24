unsafe extern "C" {
    fn getauxval(key: u64) -> u64;
}

fn main() {
    println!("vdso_test start");

    let vdso_base = unsafe { getauxval(33) };
    println!("{:#X?}", vdso_base);

    unsafe {
        libvdsoexample::init_vdso_vtable(vdso_base);
    }
    let val = libvdsoexample::get_shared().i;
    libvdsoexample::set_shared(123);
    let val2 = libvdsoexample::get_shared().i;
    println!("vdso_test end, shared value: {}, {}", val, val2);
    let val3 = libvdsoexample::get_private().i;
    libvdsoexample::set_private(456);
    let val4 = libvdsoexample::get_private().i;
    println!("vdso_test end, private value: {}, {}", val3, val4);
}
