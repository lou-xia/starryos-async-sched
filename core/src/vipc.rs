//! 基于vqueue vDSO的IPC实体接口。
//!
//! 参考vipc库的设计，提供了进程间通信所需的基本原语：
//! 实体注册、消息发送、消息接收。

use libvqueue::IPCItem;

/// vDSO管理的IPC实体。
///
/// 持有vqueue中的一个slot引用计数。
pub struct IpcEntity {
    process_id: usize,
}

impl IpcEntity {
    /// 注册一个新的IPC实体，在vqueue中分配slot。
    pub fn register() -> Result<Self, &'static str> {
        let slot = libvqueue::register_process().map_err(|_| "register failed")?;
        let process_id = slot.into_id();
        Ok(Self { process_id })
    }

    /// 从id重建实体（增加引用计数）。
    ///
    /// # Safety
    /// id必须由`Self::id()`获得。
    pub unsafe fn from_id(id: u64) -> Result<Self, &'static str> {
        let slot = libvqueue::slotref_from_id(id as usize);
        slot.clone().into_id(); // inc ref
        slot.into_id();
        Ok(Self {
            process_id: id as usize,
        })
    }

    /// 获取实体id，可在进程间传递。
    pub fn id(&self) -> u64 {
        self.process_id as u64
    }

    /// 设置调度器pid（用于通知机制）。
    pub fn set_pid(&self, pid: usize) {
        libvqueue::set_pid(self.process_id, pid);
    }

    /// 获取调度器pid。
    pub fn get_pid(&self) -> usize {
        libvqueue::get_pid(self.process_id)
    }

    /// 向目标实体发送消息。
    pub fn send_to(&self, dst_id: u64, msg_type: u64, data: [u64; 8]) -> Result<(), &'static str> {
        let item = IPCItem {
            sender: self.process_id as u64,
            msg_type,
            rep_type: 0,
            data,
        };
        libvqueue::deque_push(dst_id as usize, item).map_err(|_| "queue full")
    }

    /// 从自身队列接收消息（非阻塞：有则返回，无则返回None）。
    pub fn try_recv(&self) -> Option<IPCItem> {
        libvqueue::deque_pop(self.process_id)
    }

    /// 注册消息类型到通知id的映射。
    pub fn map_add(&self, msg_type: usize, ntf_id: usize) -> Result<(), ()> {
        libvqueue::map_add_entry(self.process_id, msg_type, ntf_id)
    }

    /// 根据消息类型查找通知id（支持通配符usize::MAX）。
    pub fn map_get(&self, msg_type: usize) -> Option<usize> {
        libvqueue::map_get_ntf_id(self.process_id, msg_type)
    }

    /// 删除消息类型到通知id的映射。
    pub fn map_pop(&self, msg_type: usize) -> Option<usize> {
        libvqueue::map_pop_ntf_id(self.process_id, msg_type)
    }
}

impl Drop for IpcEntity {
    fn drop(&mut self) {
        // 释放slot的引用计数
        let slot = unsafe { libvqueue::slotref_from_id(self.process_id) };
        core::mem::drop(slot);
    }
}
