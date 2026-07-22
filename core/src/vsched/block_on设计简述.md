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

所有 syscall 都交给独立的 SyscallTask 处理，TrapHandler 只负责分发，不再区分“可能阻塞”和“不可能阻塞”。

SyscallTask 是一个 vsched2 的内核态协程：

```text
用户任务 ecall
    → TrapHandler 保存 trap frame
    → 创建并入队 async SyscallTask
    → TrapHandler 立即返回，继续处理其他 TrapInfo
```

SyscallTask 的根入口可以写成 `async fn`，但短期内部仍然调用现有的同步 `handle_syscall`。同步函数不需要加 `.await`；它会在 SyscallTask 根 Future 被 poll 时直接执行。

```rust
async fn syscall_task(request: SyscallRequest) -> isize {
    let result = handle_syscall(&request); // 同步调用
    complete_syscall(request, result);
    0
}
```

对于不会阻塞的 syscall，`handle_syscall` 直接执行完毕，SyscallTask 一次 poll 就返回 `Ready`。

对于调用 `block_on` 后出现 `Pending` 的 syscall，将 SyscallTask 内主动调用 yield 来实现转换成线程，阻塞等待。当 `block_on` 完成后，SyscallTask 恢复成协程；处理结果写回 trap frame，然后唤醒用户任务。

因此，短期方案中不需要判断 syscall 是否会阻塞，所有 syscall 都走 SyscallTask 就行。

## 3. 最终目标

短期方案完成后，SyscallTask 已经是 vsched2 协程，因此最终不需要再更换一套 SyscallTask 模型。后续只需要按 syscall 类别，逐步将 Syscall 中的：

```rust
let result = block_on(future());
```

改成：

```rust
let result = future().await;
```

也就是把 SyscallTask 内的 future 从阻塞线程处理变成状态机处理。这需要逐 syscall 实现 block_on 到 await 的改造。



递进过程：

```text
短期：async SyscallTask + 同步 handle_syscall + block_on Pending 时按通过 yield 线程让权
  ↓ 逐类将 block_on 改为 await
最终：async SyscallTask +  await
```
