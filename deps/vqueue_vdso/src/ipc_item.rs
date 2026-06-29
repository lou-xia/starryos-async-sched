//! `IPCItem`结构。

/// 一条IPC消息的数据结构
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct IPCItem {
    /// 发送者的entity id，标识进程
    pub sender: u64,
    /// 消息类型，用于选择接收者处理消息的协程
    pub msg_type: u64,
    /// 若该消息需要回复，则回复消息发送给sender，且msg_type设为此消息的rep_type。
    ///
    /// 一般设为等待消息回复的协程id。
    ///
    /// 不需回复的消息可忽略此字段。
    pub rep_type: u64,
    /// 消息数据
    pub data: [u64; 8],
}
