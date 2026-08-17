/// vsched2 将进程 ID 0 保留给内核调度域。
pub const VSCHED_KERNEL_PROCESS_ID: usize = 0;

/// 用户地址空间尚未通过 `vsched2::process_init()` 注册时使用的无效哨兵值。
pub const VSCHED_INVALID_PROCESS_ID: usize = usize::MAX;

/// 经过校验的 vsched2 全局进程表索引。
///
/// Linux pid/tid 属于不同的命名空间；只有 `process_init()` 返回的值才能转换为该类型。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct VschedProcessId(usize);

impl VschedProcessId {
    /// 内核调度域。
    pub const KERNEL: Self = Self(VSCHED_KERNEL_PROCESS_ID);

    /// 使用 `process_init()` 返回值创建用户态进程 ID。
    pub const fn from_user_raw(raw: usize) -> Option<Self> {
        if raw == VSCHED_KERNEL_PROCESS_ID || raw == VSCHED_INVALID_PROCESS_ID {
            None
        } else {
            Some(Self(raw))
        }
    }

    /// 返回 vsched2 接口所需的进程表索引。
    pub const fn as_raw(self) -> usize {
        self.0
    }

    /// 判断该 ID 是否表示用户态调度域。
    pub const fn is_user(self) -> bool {
        self.0 != VSCHED_KERNEL_PROCESS_ID && self.0 != VSCHED_INVALID_PROCESS_ID
    }
}
