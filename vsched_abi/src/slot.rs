use core::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

use crate::VSCHED_INVALID_PROCESS_ID;

/// 共享任务槽的状态值。
pub const SHARED_TASK_FREE: usize = 0;
pub const SHARED_TASK_RESERVED: usize = 1;
pub const SHARED_TASK_LIVE: usize = 2;
pub const SHARED_TASK_RELEASING: usize = 3;

/// 共享任务的调度状态。
///
/// 这些值与 vsched2 的 `TaskState` 保持数值兼容，但不能直接复用槽位的
/// `state` 字段。`state` 表示槽位生命周期，`task_state` 才表示任务是否
/// 可以运行。两者必须分别维护。
pub const SHARED_TASK_READY: usize = 0;
pub const SHARED_TASK_RUNNING: usize = 1;
pub const SHARED_TASK_BLOCKED: usize = 2;
pub const SHARED_TASK_EXITED: usize = 3;
pub const SHARED_TASK_BLOCKING: usize = 4;

/// 共享任务当前保存的上下文类型。
///
/// `NONE` 只允许出现在槽位尚未发布上下文的短暂阶段；任务进入就绪队列
/// 前必须发布 `COROUTINE` 或 `THREAD`。`TRAP` 表示用户任务的 TrapFrame
/// 暂时由内核接管，不能被用户调度器当作普通协程上下文恢复。
pub const SHARED_CONTEXT_NONE: usize = 0;
pub const SHARED_CONTEXT_COROUTINE: usize = 1;
pub const SHARED_CONTEXT_THREAD: usize = 2;
pub const SHARED_CONTEXT_TRAP: usize = 3;

/// 表示没有执行域持有该任务上下文或就绪队列所有权。
pub const SHARED_OWNER_NONE: usize = usize::MAX;

/// 用户调度可观察的任务投影。
///
/// 这些字段都使用原子操作，因为同一 vVAR 物理页会被内核、多个地址空间和未来的多个
/// CPU 同时访问。`stack_*` 和 `context` 保存的是用户地址空间中的描述符，不能被内核当作
/// 可直接解引用的指针；它们必须受 `context_owner` 和 generation 保护。
#[repr(C)]
pub struct SharedTaskSlot {
    /// 槽位生命周期：FREE/RESERVED/LIVE/RELEASING。
    pub state: AtomicUsize,
    pub generation: AtomicUsize,
    /// 任务调度状态：Ready/Running/Blocking/Blocked/Exited。
    pub task_state: AtomicUsize,
    /// 最新保存的上下文形式：Coroutine/Thread/Trap。
    pub context_kind: AtomicUsize,
    /// 保存上下文的逻辑所有者令牌。令牌不是指针，具体取值由执行域定义。
    pub context_owner: AtomicUsize,
    /// 就绪队列的逻辑所有者令牌，用于后续防止用户态和内核态重复入队。
    pub queue_owner: AtomicUsize,
    pub process_id: AtomicUsize,
    pub priority: AtomicIsize,
    pub stack_base: AtomicUsize,
    pub stack_size: AtomicUsize,
    pub context: AtomicUsize,
    pub wake_cpu: AtomicUsize,
}

impl SharedTaskSlot {
    /// 创建一个空槽。
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(SHARED_TASK_FREE),
            generation: AtomicUsize::new(0),
            task_state: AtomicUsize::new(SHARED_TASK_EXITED),
            context_kind: AtomicUsize::new(SHARED_CONTEXT_NONE),
            context_owner: AtomicUsize::new(SHARED_OWNER_NONE),
            queue_owner: AtomicUsize::new(SHARED_OWNER_NONE),
            process_id: AtomicUsize::new(VSCHED_INVALID_PROCESS_ID),
            priority: AtomicIsize::new(0),
            stack_base: AtomicUsize::new(0),
            stack_size: AtomicUsize::new(0),
            context: AtomicUsize::new(0),
            wake_cpu: AtomicUsize::new(usize::MAX),
        }
    }

    pub(crate) fn clear_projection(&self) {
        self.task_state.store(SHARED_TASK_EXITED, Ordering::Release);
        self.context_kind
            .store(SHARED_CONTEXT_NONE, Ordering::Release);
        self.context_owner
            .store(SHARED_OWNER_NONE, Ordering::Release);
        self.queue_owner.store(SHARED_OWNER_NONE, Ordering::Release);
        self.process_id
            .store(VSCHED_INVALID_PROCESS_ID, Ordering::Release);
        self.priority.store(0, Ordering::Release);
        self.stack_base.store(0, Ordering::Release);
        self.stack_size.store(0, Ordering::Release);
        self.context.store(0, Ordering::Release);
        self.wake_cpu.store(usize::MAX, Ordering::Release);
    }

    /// 在任务仍由指定 generation 持有时，原子转换调度状态。
    pub fn compare_exchange_task_state(&self, generation: usize, from: usize, to: usize) -> bool {
        self.generation.load(Ordering::Acquire) == generation
            && self
                .task_state
                .compare_exchange(from, to, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    /// 发布最新的上下文类型和描述。
    ///
    /// 调用方必须先取得 `context_owner`，并在发布完成后再把任务置为 Ready。
    /// 这里不接受裸指针身份；`context` 只能是所属地址空间中的描述符或索引。
    pub fn publish_context_owned(
        &self,
        generation: usize,
        owner: usize,
        context_kind: usize,
        stack_base: usize,
        stack_size: usize,
        context: usize,
        wake_cpu: usize,
    ) -> bool {
        if self.generation.load(Ordering::Acquire) != generation
            || self.context_owner.load(Ordering::Acquire) != owner
            || !matches!(
                context_kind,
                SHARED_CONTEXT_COROUTINE | SHARED_CONTEXT_THREAD | SHARED_CONTEXT_TRAP
            )
        {
            return false;
        }
        self.stack_base.store(stack_base, Ordering::Relaxed);
        self.stack_size.store(stack_size, Ordering::Relaxed);
        self.context.store(context, Ordering::Relaxed);
        self.context_kind.store(context_kind, Ordering::Relaxed);
        self.wake_cpu.store(wake_cpu, Ordering::Release);
        true
    }

    /// 原子取得上下文所有权。所有权令牌由调用域生成，不能使用内核裸指针。
    pub fn try_claim_context(&self, generation: usize, owner: usize) -> bool {
        owner != SHARED_OWNER_NONE
            && self.generation.load(Ordering::Acquire) == generation
            && self
                .context_owner
                .compare_exchange(
                    SHARED_OWNER_NONE,
                    owner,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
    }

    /// 初始化任务第一次发布的上下文类型。
    ///
    /// 该操作只允许在尚未有执行域持有上下文时进行，供内核完成注册后、
    /// 任务进入用户就绪队列前建立初始协程/线程标记。
    pub fn initialize_context_kind(&self, generation: usize, context_kind: usize) -> bool {
        self.generation.load(Ordering::Acquire) == generation
            && self.context_owner.load(Ordering::Acquire) == SHARED_OWNER_NONE
            && matches!(
                context_kind,
                SHARED_CONTEXT_COROUTINE | SHARED_CONTEXT_THREAD | SHARED_CONTEXT_TRAP
            )
            && self
                .context_kind
                .compare_exchange(
                    SHARED_CONTEXT_NONE,
                    context_kind,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
    }

    /// 释放上下文所有权；错误的 owner 不能清除其他 CPU 的所有权。
    pub fn release_context(&self, generation: usize, owner: usize) -> bool {
        self.generation.load(Ordering::Acquire) == generation
            && self
                .context_owner
                .compare_exchange(
                    owner,
                    SHARED_OWNER_NONE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
    }

    /// 原子取得就绪队列所有权，防止同一个任务被用户态和内核态重复入队。
    pub fn try_claim_queue(&self, generation: usize, owner: usize) -> bool {
        owner != SHARED_OWNER_NONE
            && self.generation.load(Ordering::Acquire) == generation
            && self
                .queue_owner
                .compare_exchange(
                    SHARED_OWNER_NONE,
                    owner,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
    }

    /// 释放就绪队列所有权。
    pub fn release_queue(&self, generation: usize, owner: usize) -> bool {
        self.generation.load(Ordering::Acquire) == generation
            && self
                .queue_owner
                .compare_exchange(
                    owner,
                    SHARED_OWNER_NONE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
    }
}

impl Default for SharedTaskSlot {
    fn default() -> Self {
        Self::new()
    }
}
