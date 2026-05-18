# vsched2 集成状态

## 已完成

| 模块 | 内容 |
|------|------|
| Task 接口 | `VschedTaskImpl` — 7 个 trait 方法 + save/restore 上下文（含 Trap/Yield 分支） |
| Stack 接口 | `VschedStackImpl` — alloc/dealloc 基于全局分配器 |
| Context 接口 | `into_kernel` (ecall)、`into_user` (sret)、`into_user_context` (sret)、switch_vspace |
| VSpace 接口 | `VschedVSpaceImpl` — SATP 写 + TLB 刷新 + SUM |
| SMP 接口 | `VschedSmpImpl` — percpu cpu_id |
| UserData 接口 | `VschedUserDataImpl` — 内核→用户 vVAR 地址翻译 |
| TrapHandle 接口 | `VschedTrapHandleImpl` — 8 个 handler 的预分配池 + 动态扩容 |
| Trap 分发 | `vsched_trap_dispatcher` in starry-api — pagefault / syscall / 信号映射全覆盖 |
| 寄存器写回 | `UserGeneralRegs` 与 `axcpu::GeneralRegisters` 布局对齐，`transmute_copy` 全量写回 |
| current() 问题 | `axtask::with_current_task` RAII 临时替换 percpu 指针 |

---

## 严重 Bug

### B1: `get_handler` 丢失 trapped task 指针

**位置**: `trap.rs:74`

vsched2 的 `trap_handle()` 调用 `get_handler(trapped_task_ptr)` 传入被 trap 的任务指针。当前实现**忽略了该参数**，`TrapHandlerCoroutine::trapped_task` 始终为 0。

**影响**: `poll()` 立刻返回 `Ready(0)`，trap 事件被静默吞掉，永远不会分发到 `vsched_trap_dispatcher`。

**修复方案**: 在 `get_handler` 中取出 handler 的 `TrapHandlerCoroutine`，写入 `trapped_task`。由于 `Arc<dyn CoroutinePoll>` 的 concrete type 就是 `TrapHandlerCoroutine`，可以用 unsafe 裸指针转换：

```rust
let coro = handler.coroutine.as_ref().unwrap();
let trapped = &*(Arc::as_ptr(coro) as *const TrapHandlerCoroutine);
trapped.trapped_task.store(task as usize, Release);
```

---

## 待完成

| 位置 | 内容 | 说明 |
|------|------|------|
| `context.rs:54` | `into_user` 的 `entry = 0` | 等 vsched2 管理员在 api.rs 加 `extern "C" fn raw_run_task` 导出 |
| OS trap 入口 | 全寄存器→UserTrapFrame | 调用 `raw_trap_entry` 前把全寄存器写入 UserTrapFrame，设 `kind=Trap`，存入 `task.trap_frame` |
| OS yield 入口 | 被调用者寄存器→UserTrapFrame | 同上，设 `kind=Yield` |
| `trapframe.rs:133` | Trap 分支全量恢复 | 等用户态中断需求确定后再实现 sscratch + jr sepc |

---

## 后续需注意

| 事项 | 说明 |
|------|------|
| `main.rs:23` `init_vsched2_interfaces` | 目前注释掉了，恢复后会注册 7 个 trait 实现 + 创建 trap handler pool |
| `save_thread_context` 状态覆盖 | 无条件设 Ready，会覆盖 vsched2 刚设置的 Exited（协程完成后被 mark Dead 又被 mark Ready → 无限循环） |
| pagefault→kernel task | 如果 trapped task 没有 Thread 扩展，pagefault 被静默跳过，应加 log |
| `into_user_context` null assert | `assert_ne!(tf_ptr, 0)` 会在 trap_frame 未设置时 panic；前置条件：OS trap/yield 入口必须先设好 |
| `scause=9` (S-mode ecall) | 内核异常，走 default→SIGTRAP，可能不合理，但内核不应触发此路径 |
| SUM bit 设置 | `activate_user_aspace` 每次写 SATP 后设 SUM；如果后面有代码在 uschedule 路径频繁切换，可考虑惰性设置 |
