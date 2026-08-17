/// 共享任务槽的最大数量。
///
/// 这是阶段 B 的固定容量上限。槽位回收后可以重复使用，不会随着进程创建次数无限增长。
pub const SHARED_TASK_SLOT_COUNT: usize = 64;

/// 用户任务 ID 的标记位。
///
/// `VschedTaskImpl` 由分配器保证至少按字对齐，因此最低位可以区分任务 ID 和直接指针。
/// 编码结果只作为 vsched2 队列中的任务 ID 使用，不能解引用。
const USER_TASK_ID_TAG: usize = 1;
const USER_TASK_ID_SLOT_BITS: usize = SHARED_TASK_SLOT_COUNT.trailing_zeros() as usize;
const USER_TASK_ID_SLOT_MASK: usize = (1usize << USER_TASK_ID_SLOT_BITS) - 1;
pub(crate) const USER_TASK_ID_MAX_GENERATION: usize = usize::MAX >> (USER_TASK_ID_SLOT_BITS + 1);

/// 用户调度任务的稳定句柄。
///
/// `slot` 只在当前共享表中有意义，`generation` 用于阻止回收后复用槽位造成的
/// stale handle 重新指向新任务。用户态不能凭空构造一个可用句柄，内核 registry
/// 仍是任务身份的权威来源。
///
/// 不记得参考哪里了，好像是 Rel4 还是 Sel4 的 Capacity？
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub struct UserTaskKey {
    slot: usize,
    generation: usize,
}

impl UserTaskKey {
    /// 从共享表分配结果创建句柄。
    pub const fn new(slot: usize, generation: usize) -> Self {
        Self { slot, generation }
    }

    /// 返回槽位索引。
    pub const fn slot(self) -> usize {
        self.slot
    }

    /// 返回槽位 generation。
    pub const fn generation(self) -> usize {
        self.generation
    }
}

/// 将稳定句柄编码为 vsched2 队列中的任务 ID。
///
/// 返回的指针值不能解引用。独立标记用于区分现有的 `VschedTaskImpl` 直接指针。
pub const fn encode_task(key: UserTaskKey) -> Option<*const ()> {
    if key.slot() >= SHARED_TASK_SLOT_COUNT || key.generation() == 0 {
        return None;
    }
    let generation = key.generation();
    if generation > USER_TASK_ID_MAX_GENERATION {
        return None;
    }
    let value = ((generation << USER_TASK_ID_SLOT_BITS) | key.slot()) << 1 | USER_TASK_ID_TAG;
    Some(value as *const ())
}

/// 从 vsched2 队列中的任务 ID 恢复稳定句柄。
///
/// 该函数只检查编码格式；调用方还必须使用共享任务表校验 generation 是否仍有效。
pub fn decode_task(task: *const ()) -> Option<UserTaskKey> {
    let value = task as usize;
    if value & USER_TASK_ID_TAG == 0 {
        return None;
    }
    let value = value >> 1;
    let slot = value & USER_TASK_ID_SLOT_MASK;
    let generation = value >> USER_TASK_ID_SLOT_BITS;
    if slot >= SHARED_TASK_SLOT_COUNT || generation == 0 {
        return None;
    }
    Some(UserTaskKey::new(slot, generation))
}
