//! vipc_test: 验证基于 vqueue vDSO 的 IPC 全链路。
//!
//! 测试: basic | batch | reply | map | fork | fork_mt | signal | multi_fork
//! 多核: APP_FEATURES="qemu smp" SMP=4 make test

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use vipc::interface::{LocalEntityIf, SharedEntityIf};
use vipc::libvqueue::IPCItem;
use vipc::queue_based::QueueBasedLocalEntity;

unsafe extern "C" {
    fn getauxval(key: u64) -> u64;
    fn sched_yield() -> libc::c_int;
}

fn wait_recv(raw_id: usize) -> IPCItem {
    loop {
        if let Some(m) = vipc::libvqueue::deque_pop(raw_id) { return m; }
        unsafe { sched_yield(); }
    }
}

const MSG_COUNT: usize = 32;

fn raw_queue_id(entity: &QueueBasedLocalEntity) -> usize {
    (entity.id() & 0x00FF_FFFF_FFFF_FFFF) as usize
}

fn new_entity() -> QueueBasedLocalEntity {
    QueueBasedLocalEntity::new(false, false, None).unwrap()
}

// ============================================================
// basic — 单消息 send → pop, 队列空验证
//   输出: test_basic_ipc PASSED
// ============================================================
fn test_basic_ipc() {
    println!("=== test_basic_ipc ===");
    let client = new_entity();
    let server = new_entity();

    client.send(server.id(), 42, 0, [0xAA, 0xBB, 0xCC, 0, 0, 0, 0, 0]).unwrap();
    let msg = wait_recv(raw_queue_id(&server));
    assert_eq!(msg.sender, client.id());
    assert_eq!(msg.msg_type, 42);
    assert!(vipc::libvqueue::deque_pop(raw_queue_id(&server)).is_none());
    println!("test_basic_ipc PASSED");
}

// ============================================================
// batch — 批量 32 条, pop 验证 FIFO 顺序
//   输出: client sent 32 messages → test_batch_ipc PASSED
// ============================================================
fn test_batch_ipc() {
    println!("=== test_batch_ipc ===");
    let client = new_entity();
    let server = new_entity();

    for i in 0..MSG_COUNT {
        client.send(server.id(), (i % 4) as u64, 0, [i as u64, i as u64 * 2, 0, 0, 0, 0, 0, 0]).unwrap();
    }
    println!("client sent {} messages", MSG_COUNT);

    let mut received = 0;
    while let Some(msg) = vipc::libvqueue::deque_pop(raw_queue_id(&server)) {
        assert_eq!(msg.msg_type, (received % 4) as u64);
        assert_eq!(msg.data[0], received as u64);
        assert_eq!(msg.sender, client.id());
        received += 1;
    }
    assert_eq!(received, MSG_COUNT);
    println!("test_batch_ipc PASSED");
}

// ============================================================
// reply — 32 轮请求-回复: client→server→client
//   输出: test_reply_ipc PASSED
// ============================================================
fn test_reply_ipc() {
    println!("=== test_reply_ipc ===");
    let client = new_entity();
    let server = new_entity();
    let rc = raw_queue_id(&client);

    for i in 0..MSG_COUNT {
        client.send(server.id(), 100, 0, [i as u64, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(wait_recv(raw_queue_id(&server)).sender, client.id());
        server.send(client.id(), 200, 0, [i as u64 + 1000, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        let rep = wait_recv(rc);
        assert_eq!(rep.sender, server.id());
        assert_eq!(rep.data[0], i as u64 + 1000);
    }
    println!("test_reply_ipc PASSED");
}

// ============================================================
// map — set_pid / map_add / map_get / map_pop / 通配符
//   输出: test_map_operations PASSED
// ============================================================
fn test_map_operations() {
    println!("=== test_map_operations ===");
    let e = new_entity();
    let rid = raw_queue_id(&e);

    vipc::libvqueue::set_pid(rid, 100);
    assert_eq!(vipc::libvqueue::get_pid(rid), 100);
    vipc::libvqueue::map_add_entry(rid, 1001, 2001).unwrap();
    assert_eq!(vipc::libvqueue::map_get_ntf_id(rid, 1001), Some(2001));
    assert_eq!(vipc::libvqueue::map_pop_ntf_id(rid, 1001), Some(2001));
    assert!(vipc::libvqueue::map_get_ntf_id(rid, 1001).is_none());
    vipc::libvqueue::map_add_entry(rid, usize::MAX, 9999).unwrap();
    assert_eq!(vipc::libvqueue::map_get_ntf_id(rid, 12345), Some(9999));
    println!("test_map_operations PASSED");
}

// ============================================================
// fork — 父创建 client → fork → 子创建 server, 32 轮往返
//   VVAR 同一物理页跨进程共享
//   输出: === test_fork_ipc ===
//     [parent] client entity registered: id=0x0100000000000000
//     [child] server entity registered: id=0x0100000000000001
//     [child] sent announce to parent
//     [parent] child pid: 10
//     [parent] received announce from child: server_id=0x0100000000000001
//     [child] all messages processed, exiting
//     [parent] all 32 round-trips verified
//     test_fork_ipc PASSED
// ============================================================
fn test_fork_ipc() {
    println!("=== test_fork_ipc ===");
    let client = new_entity();
    let cid = client.id();
    let rc = raw_queue_id(&client);
    println!("[parent] client entity registered: id={:#018x}", cid);

    let pid = unsafe { libc::fork() };
    if pid == 0 {
        let server = new_entity();
        let rs = raw_queue_id(&server);
        println!("[child] server entity registered: id={:#018x}", server.id());
        server.send(cid, 1, 0, [server.id(), 0, 0, 0, 0, 0, 0, 0]).unwrap();
        println!("[child] sent announce to parent");

        for i in 0..MSG_COUNT {
            let m = wait_recv(rs);
            assert_eq!(m.sender, cid);
            server.send(cid, 200, 0, [i as u64 + 1000, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        }
        println!("[child] all messages processed, exiting");
        unsafe { libc::exit(0) };
    }
    assert!(pid > 0);
    println!("[parent] child pid: {}", pid);

    let sid = wait_recv(rc).data[0] as u64;
    println!("[parent] received announce from child: server_id={:#018x}", sid);

    for i in 0..MSG_COUNT {
        client.send(sid, 42, 0, [i as u64, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        let rep = wait_recv(rc);
        assert_eq!(rep.sender, sid);
        assert_eq!(rep.data[0], i as u64 + 1000);
    }
    println!("[parent] all {} round-trips verified", MSG_COUNT);

    let mut status: i32 = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };
    println!("test_fork_ipc PASSED");
}

// ============================================================
// fork_mt — 父创建 client → fork → 子创建 4 个 pthread worker
//   并发 deque_push, 父收 32 条验证唯一性
//   多核: pthread 线程分布在不同 CPU 上并发访问 VVAR
//   输出: === test_fork_mt_ipc ===
//     [parent] client entity registered: id=0x0100000000000000
//     [child] server entity registered: id=0x0100000000000001
//     [child] sent announce to parent
//     [child] thread 0/1/2/3 started, tid=0x...
//     [parent] child pid: 11
//     [parent] received announce from child: server_id=0x0100000000000001
//     [parent] all 32 multi-thread messages verified (unique)
//     [child] all 4 threads done, SENT_COUNT=32
//     test_fork_mt_ipc PASSED
// ============================================================
const THREAD_COUNT: usize = 4;
const MSG_PER_THREAD: usize = 8;
static SENT_COUNT: AtomicUsize = AtomicUsize::new(0);
static MT_RAW_CLIENT: AtomicUsize = AtomicUsize::new(0);
static MT_SERVER_ID: AtomicU64 = AtomicU64::new(0);
static MT_NEXT_IDX: AtomicUsize = AtomicUsize::new(0);

extern "C" fn mt_worker(_: *mut libc::c_void) -> *mut libc::c_void {
    let r = MT_RAW_CLIENT.load(Ordering::Relaxed);
    let sid = MT_SERVER_ID.load(Ordering::Relaxed);
    for _ in 0..MSG_PER_THREAD {
        let idx = MT_NEXT_IDX.fetch_add(1, Ordering::Relaxed);
        vipc::libvqueue::deque_push(r,
            IPCItem { sender: sid, msg_type: idx as u64 % 4, rep_type: 0, data: [idx as u64, 0, 0, 0, 0, 0, 0, 0] })
            .unwrap();
        SENT_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    core::ptr::null_mut()
}

fn test_fork_mt_ipc() {
    println!("=== test_fork_mt_ipc ===");
    let client = new_entity();
    let cid = client.id();
    let rc = raw_queue_id(&client);
    println!("[parent] client entity registered: id={:#018x}", cid);

    let pid = unsafe { libc::fork() };
    if pid == 0 {
        let server = new_entity();
        let sid = server.id();
        println!("[child] server entity registered: id={:#018x}", sid);
        server.send(cid, 1, 0, [sid, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        println!("[child] sent announce to parent");

        SENT_COUNT.store(0, Ordering::Relaxed);
        MT_NEXT_IDX.store(0, Ordering::Relaxed);
        MT_RAW_CLIENT.store(rc, Ordering::Relaxed);
        MT_SERVER_ID.store(sid, Ordering::Relaxed);

        for tid in 0..THREAD_COUNT {
            let mut th: libc::pthread_t = core::ptr::null_mut();
            let ret = unsafe { libc::pthread_create(&mut th, core::ptr::null(), mt_worker, core::ptr::null_mut()) };
            if ret == 0 { println!("[child] thread {} started, tid={:p}", tid, th); }
            else { println!("[child] thread {} FAILED ret={}", tid, ret); }
        }
        while SENT_COUNT.load(Ordering::Relaxed) < THREAD_COUNT * MSG_PER_THREAD { unsafe { sched_yield() }; }
        println!("[child] all {} threads done, SENT_COUNT={}", THREAD_COUNT, SENT_COUNT.load(Ordering::Relaxed));
        unsafe { libc::exit(0) };
    }
    assert!(pid > 0);
    println!("[parent] child pid: {}", pid);

    let sid = wait_recv(rc).data[0] as u64;
    println!("[parent] received announce from child: server_id={:#018x}", sid);

    let total = THREAD_COUNT * MSG_PER_THREAD;
    let mut received: Vec<u64> = (0..total as u64).map(|_| 0).collect();
    let mut count = 0;
    while count < total {
        let m = wait_recv(rc);
        assert_eq!(m.sender, sid);
        received[m.data[0] as usize] += 1;
        count += 1;
    }
    for i in 0..total { assert_eq!(received[i], 1, "msg {} dup/missing", i); }
    println!("[parent] all {} multi-thread messages verified (unique)", total);

    let mut status: i32 = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };
    println!("test_fork_mt_ipc PASSED");
}

// ============================================================
// signal — 子 sigprocmask(SIGUSR1) → sigtimedwait, 父 kill
//   绕过 StarryOS SignalOSAction::Handler 未实现
//   输出: === test_signal_ipc ===
//     [parent] client entity registered: id=0x0100000000000000
//     [child] server entity registered: id=0x0100000000000001
//     [child] ready signal, waiting for SIGUSR1...
//     [parent] child pid: 16
//     [parent] received announce from child: server_id=0x0100000000000001
//     [parent] child is ready, sending SIGUSR1
//     [parent] SIGUSR1 sent
//     [child] received SIGUSR1
//     [child] all messages processed, exiting
//     [parent] all 16 round-trips verified
//     test_signal_ipc PASSED
// ============================================================
fn test_signal_ipc() {
    println!("=== test_signal_ipc ===");
    let client = new_entity();
    let cid = client.id();
    let rc = raw_queue_id(&client);
    println!("[parent] client entity registered: id={:#018x}", cid);

    let pid = unsafe { libc::fork() };
    if pid == 0 {
        let server = new_entity();
        let rs = raw_queue_id(&server);
        println!("[child] server entity registered: id={:#018x}", server.id());
        server.send(cid, 1, 0, [server.id(), 0, 0, 0, 0, 0, 0, 0]).unwrap();

        let mut set: libc::sigset_t = unsafe { core::mem::zeroed() };
        unsafe { libc::sigaddset(&mut set, libc::SIGUSR1); }
        unsafe { libc::sigprocmask(libc::SIG_BLOCK, &set, core::ptr::null_mut()); }

        server.send(cid, 99, 0, [0; 8]).unwrap();
        println!("[child] ready signal, waiting for SIGUSR1...");

        let mut info: libc::siginfo_t = unsafe { core::mem::zeroed() };
        assert_eq!(unsafe { libc::sigtimedwait(&set, &mut info, core::ptr::null()) }, libc::SIGUSR1);
        println!("[child] received SIGUSR1");

        for i in 0..MSG_COUNT / 2 {
            assert_eq!(wait_recv(rs).sender, cid);
            server.send(cid, 200, 0, [i as u64 + 1000, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        }
        println!("[child] all messages processed, exiting");
        unsafe { libc::exit(0) };
    }
    assert!(pid > 0);
    println!("[parent] child pid: {}", pid);

    let sid = wait_recv(rc).data[0] as u64;
    println!("[parent] received announce from child: server_id={:#018x}", sid);

    while wait_recv(rc).msg_type != 99 {}
    println!("[parent] child is ready, sending SIGUSR1");
    unsafe { libc::kill(pid, libc::SIGUSR1) };
    println!("[parent] SIGUSR1 sent");

    for i in 0..MSG_COUNT / 2 {
        client.send(sid, 42, 0, [i as u64, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        let rep = wait_recv(rc);
        assert_eq!(rep.sender, sid);
        assert_eq!(rep.data[0], i as u64 + 1000);
    }
    println!("[parent] all {} round-trips verified", MSG_COUNT / 2);

    let mut status: i32 = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };
    println!("test_signal_ipc PASSED");
}

// ============================================================
// multi_fork — 父 fork 4 个子进程, 同时与所有子进程通信
//   多核: 父 + 4 子分布在不同 CPU, VVAR 被 5 个进程共享
//   输出: === test_multi_fork_ipc ===
//     [parent] client entity registered: id=0x0100000000000000
//     [parent] child 0 pid=10 / child 1 pid=11 / ...
//     [child 0-3] server entity registered / done, exiting
//     [parent] received announce from child 0-3
//     [parent] all 4 children verified
//     test_multi_fork_ipc PASSED
// ============================================================
const N_CHILDREN: usize = 4;

fn test_multi_fork_ipc() {
    println!("=== test_multi_fork_ipc ===");
    let client = new_entity();
    let cid = client.id();
    let rc = raw_queue_id(&client);
    println!("[parent] client entity registered: id={:#018x}", cid);

    let mut pids: [i32; N_CHILDREN] = [0; N_CHILDREN];
    for i in 0..N_CHILDREN {
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            let server = new_entity();
            let rs = raw_queue_id(&server);
            println!("[child {}] server entity registered: id={:#018x}", i, server.id());
            server.send(cid, 1, 0, [server.id(), i as u64, 0, 0, 0, 0, 0, 0]).unwrap();

            for _ in 0..MSG_COUNT / 2 {
                let m = wait_recv(rs);
                assert_eq!(m.sender, cid);
                server.send(cid, 200, 0, [m.data[0] + 1000, 0, 0, 0, 0, 0, 0, 0]).unwrap();
            }
            println!("[child {}] done, exiting", i);
            unsafe { libc::exit(0) };
        }
        pids[i] = pid;
        println!("[parent] child {} pid={}", i, pid);
    }

    let mut sids: [u64; N_CHILDREN] = [0; N_CHILDREN];
    for _ in 0..N_CHILDREN {
        let m = wait_recv(rc);
        let idx = m.data[1] as usize;
        sids[idx] = m.data[0];
        println!("[parent] received announce from child {}: server_id={:#018x}", idx, sids[idx]);
    }

    for ci in 0..N_CHILDREN {
        for i in 0..MSG_COUNT / 2 {
            let base = (ci * MSG_COUNT / 2 + i) as u64;
            client.send(sids[ci], 42, 0, [base, 0, 0, 0, 0, 0, 0, 0]).unwrap();
            let rep = wait_recv(rc);
            assert_eq!(rep.sender, sids[ci]);
            assert_eq!(rep.data[0], base + 1000);
        }
    }
    println!("[parent] all {} children verified", N_CHILDREN);

    for &pid in &pids {
        let mut status: i32 = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };
    }
    println!("test_multi_fork_ipc PASSED");
}

fn main() {
    println!("vipc_test start");
    let vdso_base = unsafe { getauxval(33) };
    assert_ne!(vdso_base, 0, "AT_SYSINFO_EHDR should not be 0");
    unsafe { vipc::libvqueue::init_vdso_vtable(vdso_base) };

    test_basic_ipc();
    test_batch_ipc();
    test_reply_ipc();
    test_map_operations();
    test_fork_ipc();
    test_fork_mt_ipc();
    test_signal_ipc();
    test_multi_fork_ipc();

    println!("vipc_test: ALL TESTS PASSED");
}
