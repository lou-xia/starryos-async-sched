use libvqueue::IPCItem;

unsafe extern "C" {
    fn getauxval(key: u64) -> u64;
}

fn main() {
    println!("vqueue_test start");

    let vdso_base = unsafe { getauxval(33) };
    println!("vdso_base: {:#X}", vdso_base);
    assert_ne!(vdso_base, 0, "AT_SYSINFO_EHDR should not be 0");

    unsafe {
        libvqueue::init_vdso_vtable(vdso_base);
    }

    // Test register_process
    let slot_ref = libvqueue::register_process().expect("register_process failed");
    let process_id = slot_ref.into_id();
    println!("process registered, id: {}", process_id);
    assert!(process_id > 0);

    // Test deque_push / deque_pop
    let item = IPCItem {
        sender: 1,
        msg_type: 42,
        rep_type: 0,
        data: [1, 2, 3, 4, 5, 6, 7, 8],
    };
    libvqueue::deque_push(process_id, item).expect("deque_push failed");

    let popped = libvqueue::deque_pop(process_id).expect("deque_pop should return Some");
    assert_eq!(popped.sender, 1);
    assert_eq!(popped.msg_type, 42);
    assert_eq!(popped.data, [1, 2, 3, 4, 5, 6, 7, 8]);
    println!("deque push/pop works: sender={}, msg_type={}", popped.sender, popped.msg_type);

    // Test deque_pop on empty queue
    let empty = libvqueue::deque_pop(process_id);
    assert!(empty.is_none(), "deque should be empty after pop");
    println!("deque_pop on empty returns None: ok");

    // Test set_pid / get_pid
    let prev_pid = libvqueue::get_pid(process_id);
    println!("prev_pid: {}", prev_pid);
    libvqueue::set_pid(process_id, 100);
    let new_pid = libvqueue::get_pid(process_id);
    assert_eq!(new_pid, 100);
    println!("set_pid/get_pid works: {}", new_pid);

    // Test map_add_entry / map_get_ntf_id / map_pop_ntf_id
    let result = libvqueue::map_add_entry(process_id, 1001, 2001);
    assert!(result.is_ok(), "map_add_entry failed");
    let ntf = libvqueue::map_get_ntf_id(process_id, 1001);
    assert_eq!(ntf, Some(2001));
    println!("map_add_entry/map_get_ntf_id works: ntf_id={}", ntf.unwrap());

    let popped_ntf = libvqueue::map_pop_ntf_id(process_id, 1001);
    assert_eq!(popped_ntf, Some(2001));
    let after_pop = libvqueue::map_get_ntf_id(process_id, 1001);
    assert!(after_pop.is_none(), "should be removed after pop");
    println!("map_pop_ntf_id works");

    // Test wildcard (usize::MAX) entry
    let _ = libvqueue::map_add_entry(process_id, usize::MAX, 9999);
    let wildcard_ntf = libvqueue::map_get_ntf_id(process_id, 12345);
    assert_eq!(wildcard_ntf, Some(9999));
    println!("wildcard map_get_ntf_id works: {}", wildcard_ntf.unwrap());

    println!("vqueue_test: all tests passed");
}
