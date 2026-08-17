use core::sync::atomic::Ordering;

use crate::*;

#[test]
fn process_id_rejects_reserved_values() {
    assert!(VschedProcessId::from_user_raw(VSCHED_KERNEL_PROCESS_ID).is_none());
    assert!(VschedProcessId::from_user_raw(VSCHED_INVALID_PROCESS_ID).is_none());

    let id = VschedProcessId::from_user_raw(1).expect("用户进程槽 1 应当有效");
    assert_eq!(id.as_raw(), 1);
    assert!(id.is_user());
}

#[test]
fn shared_task_generation_invalidates_stale_key() {
    let table = SharedTaskTable::new();
    let process = VschedProcessId::from_user_raw(1).unwrap();
    let old = table.allocate(process, 3).unwrap();
    assert!(table.is_live(old));
    assert_eq!(table.process_id(old), Some(process));
    assert!(table.release(old));
    assert!(!table.is_live(old));

    let new = table.allocate(process, 4).unwrap();
    assert_eq!(new.slot(), old.slot());
    assert_ne!(new.generation(), old.generation());
    assert!(!table.release(old));
    assert!(table.is_live(new));
}

#[test]
fn shared_task_state_and_ownership_are_generation_checked() {
    let table = SharedTaskTable::new();
    let process = VschedProcessId::from_user_raw(1).unwrap();
    let key = table.allocate(process, 3).unwrap();
    let slot = &table.slots[key.slot()];

    assert!(table.initialize_context_kind(key, SHARED_CONTEXT_COROUTINE));
    assert!(!table.initialize_context_kind(key, SHARED_CONTEXT_THREAD));
    assert!(table.compare_exchange_task_state(key, SHARED_TASK_READY, SHARED_TASK_RUNNING));
    assert!(!table.compare_exchange_task_state(key, SHARED_TASK_READY, SHARED_TASK_BLOCKED));

    let context_owner = 7;
    let other_owner = 8;
    assert!(table.try_claim_context(key, context_owner));
    assert!(!table.try_claim_context(key, other_owner));
    assert!(!table.publish_context_owned(
        key,
        other_owner,
        SHARED_CONTEXT_THREAD,
        0x1000,
        0x2000,
        0x3000,
        2,
    ));
    assert!(table.publish_context_owned(
        key,
        context_owner,
        SHARED_CONTEXT_THREAD,
        0x1000,
        0x2000,
        0x3000,
        2,
    ));
    assert_eq!(
        slot.context_kind.load(Ordering::Acquire),
        SHARED_CONTEXT_THREAD
    );
    assert_eq!(slot.stack_base.load(Ordering::Acquire), 0x1000);
    assert_eq!(slot.stack_size.load(Ordering::Acquire), 0x2000);
    assert_eq!(slot.context.load(Ordering::Acquire), 0x3000);
    assert_eq!(slot.wake_cpu.load(Ordering::Acquire), 2);
    assert!(!table.release_context(key, other_owner));
    assert!(table.release_context(key, context_owner));

    assert!(table.try_claim_queue(key, context_owner));
    assert!(!table.try_claim_queue(key, other_owner));
    assert!(!table.release_queue(key, other_owner));
    assert!(table.release_queue(key, context_owner));

    assert!(table.release(key));
    assert!(!table.compare_exchange_task_state(key, SHARED_TASK_RUNNING, SHARED_TASK_READY));
    assert!(!table.try_claim_context(key, context_owner));
    assert!(!table.try_claim_queue(key, context_owner));
}

#[test]
fn task_id_preserves_slot_and_generation() {
    let key = UserTaskKey::new(3, 9);
    let task = encode_task(key).expect("valid task key must be encodable");
    assert_eq!(decode_task(task), Some(key));
    assert_eq!(decode_task(core::ptr::null()), None);
    assert_eq!(decode_task(0xffff_ffc0_0000_0000usize as *const ()), None);
    let direct = &0usize as *const usize as *const ();
    assert_eq!(decode_task(direct), None);
    assert_eq!(
        encode_task(UserTaskKey::new(SHARED_TASK_SLOT_COUNT, 1)),
        None
    );
    assert_eq!(encode_task(UserTaskKey::new(0, 0)), None);
}
