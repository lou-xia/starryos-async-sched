# `block_on` 设计简述

## 1. 现在的问题

可能阻塞的 syscall （比如 wait 系统调用）直接运行在 TrapHandler 的调用栈上：

1. 用户任务发起系统调用
2. TrapHandler
3. handle_syscall
4. block_on(Future)
5. Future Pending

此时会阻塞 TrapHandler，导致后续的 Trap 无法处理，出现卡死。

此外，block_on 内的 Waker 唤醒的不是共享调度器的任务，需要修改成能够直接唤醒共享调度器的任务

## 2. 短期方案

不重写现有同步 syscall，只把可能阻塞的调用链从 TrapHandler 中移出去。

将系统调用分为两类：可能阻塞的和不可能阻塞的。可能阻塞的指存在等待可能的 syscall，例如 wait、pipe、poll、futex、sleep、TTY、socket 和异步磁盘 I/O。

需要一个专门处理可能阻塞的系统调用的任务 SyscallTask，它是普通的 vsched2 线程任务，保存所需的进程信息和 trap frame。

1. 用户任务发起 syscall，变成 blocked 状态
2. TrapHandler 判断是否是可能阻塞的 Syscall
3. 如果不是就直接在 TrapHandler 中处理
4. 如果是就创建 SyscallTask 任务，以线程形式运行
5. SyscallTask 中调用 block_on(Future)
6. Future 返回 Pending:  
    1. 阻塞 SyscallTask，TrapHandler 继续运行
    2. 等待 Waker 唤醒 SyscallTask 后，重新加入 vsched2 的调度队列
7. SyscallTask 处理完成，唤醒用户任务

总结而言，就是把用到 block_on 的系统调用创建新的任务调用 block_on，与 TrapHandler 分离。

## 3. 长期方案

将 SyscallTask 转换成协程，通过 vsched2 协程实现整个 syscall 过程，类似 Async-OS 里的 run_task 函数，逐步消除 block_on

短期：

```rust
fn syscall_task() {
    let result = block_on(future());
    back_to_user(result);
}
```

最终：

```rust
async fn syscall_coroutine() {
    let result = future().await;
    back_to_user(result);
}
```

此时，block_on 的参数 future 也作为 vsched2 的协程参与调度。




