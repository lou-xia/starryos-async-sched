//! StarryOS 用户态的 vsched2 任务适配层。
//!
//! 当前阶段只提供共享任务字段和 Task VTABLE。寄存器、栈及协程 continuation
//! 的保存与恢复属于下一阶段，在对应协议完成前不会启用实际用户态切换。

#![no_std]

use core::task::Poll;

use libvsched2::{Task, TaskState};
use vsched_abi::{
    SHARED_CONTEXT_COROUTINE, SHARED_TASK_BLOCKED, SHARED_TASK_BLOCKING, SHARED_TASK_EXITED,
    SHARED_TASK_READY, SHARED_TASK_RUNNING, SharedTaskTable, UserTaskKey, VschedProcessId,
    decode_task,
};

/// 用户地址空间中的任务视图。
///
/// 该类型不保存数据。`self` 的地址就是 vsched2 队列中的编码任务 ID，任务字段统一
/// 存放在 StarryOS vVAR 的共享任务槽中。
pub struct UserTask;

impl UserTask {
    fn raw(&self) -> *const () {
        self as *const Self as *const ()
    }

    fn key(&self) -> UserTaskKey {
        decode_task(self.raw()).expect("用户任务 ID 编码无效")
    }

    fn table() -> &'static SharedTaskTable {
        let table = libstarry_vsched::user_task_table();
        assert!(!table.is_null(), "共享任务表尚未初始化");
        unsafe { &*table }
    }

    fn checked_key(&self) -> UserTaskKey {
        let key = self.key();
        assert!(Self::table().is_live(key), "用户任务 ID 已失效");
        key
    }
}

/// 返回当前 CPU 上的用户任务视图。
///
/// 若 vsched2 尚未设置当前任务、当前任务不是编码用户任务，或对应槽位已经失效，
/// 则返回 `None`。
pub fn current_task() -> Option<&'static UserTask> {
    let raw = libvsched2::current_task_ptr();
    task(raw)
}

/// 将编码任务 ID 转换为经过 generation 校验的用户任务视图。
pub fn task(raw: *const ()) -> Option<&'static UserTask> {
    let key = decode_task(raw)?;
    UserTask::table()
        .is_live(key)
        .then(|| unsafe { &*(raw as *const UserTask) })
}

/// 为当前用户地址空间中的 vsched2 实例注册 Task VTABLE。
///
/// 每次 exec 后只调用一次。该函数不会运行调度器，也不会切换任务。
pub fn init_task_vtable() {
    libvsched2::init_vtable_Task::<UserTask>();
}

impl Task for UserTask {
    fn state(&self) -> TaskState {
        let key = self.checked_key();
        state_from_raw(Self::table().task_state(key).expect("读取用户任务状态失败"))
    }

    fn set_state(&self, state: TaskState) -> TaskState {
        let key = self.checked_key();
        let old = Self::table()
            .swap_task_state(key, state_to_raw(state))
            .expect("更新用户任务状态失败");
        state_from_raw(old)
    }

    fn match_set_state(
        &self,
        state_from_ready: TaskState,
        state_from_running: TaskState,
        state_from_blocked: TaskState,
        state_from_exited: TaskState,
        state_from_blocking: TaskState,
    ) -> TaskState {
        let key = self.checked_key();
        let old = Self::table()
            .match_set_task_state(
                key,
                state_to_raw(state_from_ready),
                state_to_raw(state_from_running),
                state_to_raw(state_from_blocked),
                state_to_raw(state_from_exited),
                state_to_raw(state_from_blocking),
            )
            .expect("按当前状态更新用户任务失败");
        state_from_raw(old)
    }

    fn priority(&self) -> isize {
        let key = self.checked_key();
        Self::table().priority(key).expect("读取用户任务优先级失败")
    }

    fn is_coroutine(&self) -> bool {
        let key = self.checked_key();
        Self::table()
            .context_kind(key)
            .expect("读取用户任务上下文类型失败")
            == SHARED_CONTEXT_COROUTINE
    }

    fn is_kernel(&self) -> bool {
        self.checked_key();
        false
    }

    fn pid(&self) -> usize {
        let key = self.checked_key();
        Self::table()
            .process_id(key)
            .expect("读取用户任务进程 ID 失败")
            .as_raw()
    }

    fn set_pid(&self, pid: usize) {
        let key = self.checked_key();
        let pid = VschedProcessId::from_user_raw(pid).expect("用户任务不能使用保留进程 ID");
        assert!(
            Self::table().set_process_id(key, pid),
            "更新用户任务进程 ID 失败"
        );
    }

    fn resched(&self) {
        panic!("用户态主动让权将在阶段 D 实现")
    }

    fn restore_context(&self) {
        panic!("用户态线程上下文恢复将在阶段 D 实现")
    }

    fn poll(&self) -> Poll<isize> {
        panic!("用户态协程轮询将在阶段 D 实现")
    }

    fn thread_stack(&self) -> *mut () {
        panic!("用户态线程栈适配将在阶段 D 实现")
    }

    fn set_return_value(&self, _value: isize) {
        panic!("用户态协程返回值将在阶段 D 实现")
    }

    fn dealloc(&self) {
        panic!("用户任务回收仍由内核完成")
    }
}

fn state_to_raw(state: TaskState) -> usize {
    match state {
        TaskState::Ready => SHARED_TASK_READY,
        TaskState::Running => SHARED_TASK_RUNNING,
        TaskState::Blocked => SHARED_TASK_BLOCKED,
        TaskState::Exited => SHARED_TASK_EXITED,
        TaskState::Blocking => SHARED_TASK_BLOCKING,
    }
}

fn state_from_raw(state: usize) -> TaskState {
    match state {
        SHARED_TASK_READY => TaskState::Ready,
        SHARED_TASK_RUNNING => TaskState::Running,
        SHARED_TASK_BLOCKED => TaskState::Blocked,
        SHARED_TASK_EXITED => TaskState::Exited,
        SHARED_TASK_BLOCKING => TaskState::Blocking,
        _ => panic!("共享任务状态值无效: {state}"),
    }
}
