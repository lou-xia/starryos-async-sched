use core::sync::atomic::Ordering;

use crate::{
    SHARED_CONTEXT_COROUTINE, SHARED_CONTEXT_NONE, SHARED_CONTEXT_THREAD, SHARED_CONTEXT_TRAP,
    SHARED_OWNER_NONE, SHARED_TASK_BLOCKED, SHARED_TASK_BLOCKING, SHARED_TASK_EXITED,
    SHARED_TASK_FREE, SHARED_TASK_LIVE, SHARED_TASK_READY, SHARED_TASK_RELEASING,
    SHARED_TASK_RESERVED, SHARED_TASK_RUNNING, SHARED_TASK_SLOT_COUNT, SharedTaskSlot,
    USER_TASK_ID_MAX_GENERATION, UserTaskKey, VschedProcessId,
};

/// vVAR 中的固定容量共享任务表。
///
/// 表本身只负责原子槽分配和 generation 校验；任务到内核对象的映射由 StarryOS 内核
/// registry 单独维护，避免把内核裸指针写入共享页。
#[repr(C)]
pub struct SharedTaskTable {
    pub slots: [SharedTaskSlot; SHARED_TASK_SLOT_COUNT],
}

impl SharedTaskTable {
    /// 创建空任务表。
    pub const fn new() -> Self {
        Self {
            slots: [const { SharedTaskSlot::new() }; SHARED_TASK_SLOT_COUNT],
        }
    }

    fn slot(&self, key: UserTaskKey) -> Option<&SharedTaskSlot> {
        let slot = self.slots.get(key.slot())?;
        (slot.generation.load(Ordering::Acquire) == key.generation()
            && slot.state.load(Ordering::Acquire) == SHARED_TASK_LIVE)
            .then_some(slot)
    }

    fn next_generation(slot: &SharedTaskSlot) -> usize {
        let generation = slot
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        if generation == 0 || generation > USER_TASK_ID_MAX_GENERATION {
            slot.generation.store(1, Ordering::Release);
            1
        } else {
            generation
        }
    }

    /// 分配一个属于指定 vsched2 进程槽的任务槽。
    pub fn allocate(&self, process_id: VschedProcessId, priority: isize) -> Option<UserTaskKey> {
        if !process_id.is_user() {
            return None;
        }
        for (slot_index, slot) in self.slots.iter().enumerate() {
            if slot
                .state
                .compare_exchange(
                    SHARED_TASK_FREE,
                    SHARED_TASK_RESERVED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                continue;
            }

            let generation = Self::next_generation(slot);
            slot.task_state.store(SHARED_TASK_READY, Ordering::Relaxed);
            slot.context_kind
                .store(SHARED_CONTEXT_NONE, Ordering::Relaxed);
            slot.context_owner
                .store(SHARED_OWNER_NONE, Ordering::Relaxed);
            slot.queue_owner.store(SHARED_OWNER_NONE, Ordering::Relaxed);
            slot.process_id
                .store(process_id.as_raw(), Ordering::Relaxed);
            slot.priority.store(priority, Ordering::Relaxed);
            slot.stack_base.store(0, Ordering::Relaxed);
            slot.stack_size.store(0, Ordering::Relaxed);
            slot.context.store(0, Ordering::Relaxed);
            slot.wake_cpu.store(usize::MAX, Ordering::Relaxed);
            slot.state.store(SHARED_TASK_LIVE, Ordering::Release);
            return Some(UserTaskKey::new(slot_index, generation));
        }
        None
    }

    /// 使句柄失效并回收其槽位。
    pub fn release(&self, key: UserTaskKey) -> bool {
        let Some(slot) = self.slots.get(key.slot()) else {
            return false;
        };
        if slot.generation.load(Ordering::Acquire) != key.generation()
            || slot
                .state
                .compare_exchange(
                    SHARED_TASK_LIVE,
                    SHARED_TASK_RELEASING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
        {
            return false;
        }

        // 先推进 generation，再发布 FREE，避免旧句柄在槽位重用时重新生效。
        Self::next_generation(slot);
        slot.clear_projection();
        slot.state.store(SHARED_TASK_FREE, Ordering::Release);
        true
    }

    /// 判断句柄是否仍指向活动槽位。
    pub fn is_live(&self, key: UserTaskKey) -> bool {
        self.slot(key).is_some()
    }

    /// 原子转换共享任务的调度状态。
    pub fn compare_exchange_task_state(&self, key: UserTaskKey, from: usize, to: usize) -> bool {
        self.slot(key)
            .is_some_and(|slot| slot.compare_exchange_task_state(key.generation(), from, to))
    }

    /// 读取共享任务的调度状态。
    pub fn task_state(&self, key: UserTaskKey) -> Option<usize> {
        Some(self.slot(key)?.task_state.load(Ordering::Acquire))
    }

    /// 交换共享任务的调度状态并返回旧值。
    pub fn swap_task_state(&self, key: UserTaskKey, new_state: usize) -> Option<usize> {
        let slot = self.slot(key)?;
        Some(slot.task_state.swap(new_state, Ordering::AcqRel))
    }

    /// 按当前状态选择目标状态，并在一次原子操作中完成更新。
    pub fn match_set_task_state(
        &self,
        key: UserTaskKey,
        state_from_ready: usize,
        state_from_running: usize,
        state_from_blocked: usize,
        state_from_exited: usize,
        state_from_blocking: usize,
    ) -> Option<usize> {
        let slot = self.slot(key)?;
        let mut current = slot.task_state.load(Ordering::Acquire);
        loop {
            let next = match current {
                SHARED_TASK_READY => state_from_ready,
                SHARED_TASK_RUNNING => state_from_running,
                SHARED_TASK_BLOCKED => state_from_blocked,
                SHARED_TASK_EXITED => state_from_exited,
                SHARED_TASK_BLOCKING => state_from_blocking,
                _ => return None,
            };
            match slot.task_state.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(current),
                Err(observed) => current = observed,
            }
        }
    }

    /// 读取共享任务优先级。
    pub fn priority(&self, key: UserTaskKey) -> Option<isize> {
        Some(self.slot(key)?.priority.load(Ordering::Acquire))
    }

    /// 读取共享任务上下文类型。
    pub fn context_kind(&self, key: UserTaskKey) -> Option<usize> {
        Some(self.slot(key)?.context_kind.load(Ordering::Acquire))
    }

    /// 更新共享任务保存的上下文类型。
    pub fn set_context_kind(&self, key: UserTaskKey, context_kind: usize) -> bool {
        if !matches!(
            context_kind,
            SHARED_CONTEXT_COROUTINE | SHARED_CONTEXT_THREAD | SHARED_CONTEXT_TRAP
        ) {
            return false;
        }
        let Some(slot) = self.slot(key) else {
            return false;
        };
        slot.context_kind.store(context_kind, Ordering::Release);
        true
    }

    /// 更新共享任务所属的 vsched2 进程槽。
    pub fn set_process_id(&self, key: UserTaskKey, process_id: VschedProcessId) -> bool {
        if !process_id.is_user() {
            return false;
        }
        let Some(slot) = self.slot(key) else {
            return false;
        };
        slot.process_id
            .store(process_id.as_raw(), Ordering::Release);
        true
    }

    /// 记录任务最近一次运行或被唤醒时所在的 CPU。
    pub fn set_wake_cpu(&self, key: UserTaskKey, cpu_id: usize) -> bool {
        let Some(slot) = self.slot(key) else {
            return false;
        };
        slot.wake_cpu.store(cpu_id, Ordering::Release);
        true
    }

    /// 读取任务最近一次运行或被唤醒时所在的 CPU。
    pub fn wake_cpu(&self, key: UserTaskKey) -> Option<usize> {
        Some(self.slot(key)?.wake_cpu.load(Ordering::Acquire))
    }

    /// 取得任务上下文所有权。
    pub fn try_claim_context(&self, key: UserTaskKey, owner: usize) -> bool {
        self.slot(key)
            .is_some_and(|slot| slot.try_claim_context(key.generation(), owner))
    }

    /// 初始化任务第一次发布的上下文类型。
    pub fn initialize_context_kind(&self, key: UserTaskKey, context_kind: usize) -> bool {
        self.slot(key)
            .is_some_and(|slot| slot.initialize_context_kind(key.generation(), context_kind))
    }

    /// 发布由指定 owner 保存的用户上下文。
    pub fn publish_context_owned(
        &self,
        key: UserTaskKey,
        owner: usize,
        context_kind: usize,
        stack_base: usize,
        stack_size: usize,
        context: usize,
        wake_cpu: usize,
    ) -> bool {
        self.slot(key).is_some_and(|slot| {
            slot.publish_context_owned(
                key.generation(),
                owner,
                context_kind,
                stack_base,
                stack_size,
                context,
                wake_cpu,
            )
        })
    }

    /// 释放任务上下文所有权。
    pub fn release_context(&self, key: UserTaskKey, owner: usize) -> bool {
        self.slot(key)
            .is_some_and(|slot| slot.release_context(key.generation(), owner))
    }

    /// 取得就绪队列所有权。
    pub fn try_claim_queue(&self, key: UserTaskKey, owner: usize) -> bool {
        self.slot(key)
            .is_some_and(|slot| slot.try_claim_queue(key.generation(), owner))
    }

    /// 释放就绪队列所有权。
    pub fn release_queue(&self, key: UserTaskKey, owner: usize) -> bool {
        self.slot(key)
            .is_some_and(|slot| slot.release_queue(key.generation(), owner))
    }

    /// 查询活动槽所属的 vsched2 进程。
    pub fn process_id(&self, key: UserTaskKey) -> Option<VschedProcessId> {
        let slot = self.slot(key)?;
        VschedProcessId::from_user_raw(slot.process_id.load(Ordering::Acquire))
    }

    /// 发布用户 context 的描述信息。
    ///
    /// 这是兼容阶段 B 的无所有权接口。新的用户调度路径应优先使用
    /// [`SharedTaskSlot::publish_context_owned`]，避免在多个 CPU 间覆盖上下文。
    pub fn publish_context(
        &self,
        key: UserTaskKey,
        stack_base: usize,
        stack_size: usize,
        context: usize,
        wake_cpu: usize,
    ) -> bool {
        let Some(slot) = self.slot(key) else {
            return false;
        };
        slot.stack_base.store(stack_base, Ordering::Relaxed);
        slot.stack_size.store(stack_size, Ordering::Relaxed);
        slot.context.store(context, Ordering::Relaxed);
        slot.wake_cpu.store(wake_cpu, Ordering::Release);
        true
    }
}

impl Default for SharedTaskTable {
    fn default() -> Self {
        Self::new()
    }
}
