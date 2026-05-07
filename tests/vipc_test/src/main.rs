use vipc::libvqueue::IPCItem;
use vipc::interface::{IPCSharedEntity, LocalEntityIf, SharedEntityIf};
use vipc::queue_based::QueueBasedLocalEntity;

unsafe extern "C" {
    fn getauxval(key: u64) -> u64;
}

const MSG_COUNT: usize = 32;

fn raw_queue_id(entity: &QueueBasedLocalEntity) -> usize {
    (entity.id() & 0x00FF_FFFF_FFFF_FFFF) as usize
}

fn test_basic_ipc() {
    println!("=== test_basic_ipc ===");

    let client = QueueBasedLocalEntity::new(false, false, None).unwrap();
    let server = QueueBasedLocalEntity::new(false, false, None).unwrap();

    let client_id = client.id();
    let server_id = server.id();
    let send_data = [0xAA, 0xBB, 0xCC, 0, 0, 0, 0, 0];

    client.send(server_id, 42, 0, send_data).unwrap();

    let msg = vipc::libvqueue::deque_pop(raw_queue_id(&server)).expect("server should receive msg");
    assert_eq!(msg.sender, client_id);
    assert_eq!(msg.msg_type, 42);
    assert_eq!(msg.data, send_data);
    println!("basic send/recv OK: sender={}, msg_type={}", msg.sender, msg.msg_type);

    assert!(vipc::libvqueue::deque_pop(raw_queue_id(&server)).is_none());
    println!("test_basic_ipc PASSED");
}

fn test_batch_ipc() {
    println!("=== test_batch_ipc ===");

    let client = QueueBasedLocalEntity::new(false, false, None).unwrap();
    let server = QueueBasedLocalEntity::new(false, false, None).unwrap();

    let client_id = client.id();
    let server_id = server.id();

    for i in 0..MSG_COUNT {
        let data = [i as u64, i as u64 * 2, 0, 0, 0, 0, 0, 0];
        client.send(server_id, (i % 4) as u64, 0, data).unwrap();
    }
    println!("client sent {} messages", MSG_COUNT);

    let mut received = 0;
    let raw_id = raw_queue_id(&server);
    while let Some(msg) = vipc::libvqueue::deque_pop(raw_id) {
        assert_eq!(msg.msg_type, (received % 4) as u64);
        assert_eq!(msg.data[0], received as u64);
        assert_eq!(msg.data[1], received as u64 * 2);
        assert_eq!(msg.sender, client_id);
        received += 1;
    }
    assert_eq!(received, MSG_COUNT, "should receive {} messages", MSG_COUNT);
    println!("test_batch_ipc PASSED");
}

fn test_reply_ipc() {
    println!("=== test_reply_ipc ===");

    let client = QueueBasedLocalEntity::new(false, false, None).unwrap();
    let server = QueueBasedLocalEntity::new(false, false, None).unwrap();

    let client_id = client.id();
    let server_id = server.id();
    let raw_client = raw_queue_id(&client);

    for i in 0..MSG_COUNT {
        let req_data = [i as u64, 0, 0, 0, 0, 0, 0, 0];
        client.send(server_id, 100, 0, req_data).unwrap();

        let req = vipc::libvqueue::deque_pop(raw_queue_id(&server)).expect("server should receive");
        assert_eq!(req.sender, client_id);
        assert_eq!(req.msg_type, 100);

        let rep_data = [i as u64 + 1000, 0, 0, 0, 0, 0, 0, 0];
        server.send(client_id, 200, 0, rep_data).unwrap();

        let rep = vipc::libvqueue::deque_pop(raw_client).expect("client should receive reply");
        assert_eq!(rep.sender, server_id);
        assert_eq!(rep.msg_type, 200);
        assert_eq!(rep.data[0], i as u64 + 1000);
    }
    println!("test_reply_ipc PASSED");
}

fn test_map_operations() {
    println!("=== test_map_operations ===");

    let entity = QueueBasedLocalEntity::new(false, false, None).unwrap();
    let raw_id = raw_queue_id(&entity);

    vipc::libvqueue::set_pid(raw_id, 100);
    assert_eq!(vipc::libvqueue::get_pid(raw_id), 100);

    assert!(vipc::libvqueue::map_add_entry(raw_id, 1001, 2001).is_ok());
    assert_eq!(vipc::libvqueue::map_get_ntf_id(raw_id, 1001), Some(2001));

    assert_eq!(vipc::libvqueue::map_pop_ntf_id(raw_id, 1001), Some(2001));
    assert!(vipc::libvqueue::map_get_ntf_id(raw_id, 1001).is_none());

    vipc::libvqueue::map_add_entry(raw_id, usize::MAX, 9999).unwrap();
    assert_eq!(vipc::libvqueue::map_get_ntf_id(raw_id, 12345), Some(9999));

    println!("test_map_operations PASSED");
}

fn test_fork_ipc() {
    println!("=== test_fork_ipc ===");

    let client = QueueBasedLocalEntity::new(false, false, None).unwrap();
    let client_id = client.id();
    println!("[parent] client entity registered: id={:#018x}", client_id);

    let pid = unsafe { libc::fork() };
    if pid == 0 {
        let server = QueueBasedLocalEntity::new(false, false, None).unwrap();
        let server_id = server.id();
        println!("[child] server entity registered: id={:#018x}", server_id);

        let raw_server = raw_queue_id(&server);
        let announce_data = [server_id, 0, 0, 0, 0, 0, 0, 0];
        server.send(client_id, 1, 0, announce_data).unwrap();
        println!("[child] sent announce to parent");

        for i in 0..MSG_COUNT {
            loop {
                if let Some(m) = vipc::libvqueue::deque_pop(raw_server) {
                    assert_eq!(m.sender, client_id);
                    assert_eq!(m.msg_type, 42);
                    assert_eq!(m.data[0], i as u64);
                    let rep_data = [i as u64 + 1000, 0, 0, 0, 0, 0, 0, 0];
                    server.send(client_id, 200, 0, rep_data).unwrap();
                    break;
                }
            }
        }
        println!("[child] all messages processed, exiting");
        unsafe { libc::exit(0) };
    } else {
        assert!(pid > 0, "fork failed: pid={}", pid);
        println!("[parent] child pid: {}", pid);

        let raw_client = raw_queue_id(&client);
        let announce = loop {
            if let Some(m) = vipc::libvqueue::deque_pop(raw_client) {
                break m;
            }
        };
        let server_id = announce.data[0] as u64;
        assert_eq!(announce.sender, server_id);
        assert_eq!(announce.msg_type, 1);
        println!("[parent] received announce from child: server_id={:#018x}", server_id);

        for i in 0..MSG_COUNT {
            let data = [i as u64, 0, 0, 0, 0, 0, 0, 0];
            client.send(server_id, 42, 0, data).unwrap();
            loop {
                if let Some(rep) = vipc::libvqueue::deque_pop(raw_client) {
                    assert_eq!(rep.sender, server_id);
                    assert_eq!(rep.msg_type, 200);
                    assert_eq!(rep.data[0], i as u64 + 1000);
                    break;
                }
            }
        }
        println!("[parent] all {} round-trips verified", MSG_COUNT);

        let mut status: i32 = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };
        println!("test_fork_ipc PASSED");
    }
}

fn main() {
    println!("vipc_test start");

    let vdso_base = unsafe { getauxval(33) };
    println!("vdso_base: {:#X}", vdso_base);
    assert_ne!(vdso_base, 0, "AT_SYSINFO_EHDR should not be 0");

    unsafe {
        vipc::libvqueue::init_vdso_vtable(vdso_base);
    }
    println!("vDSO vtable initialized");

    test_basic_ipc();
    test_batch_ipc();
    test_reply_ipc();
    test_map_operations();
    test_fork_ipc();

    println!("vipc_test: ALL TESTS PASSED");
}
