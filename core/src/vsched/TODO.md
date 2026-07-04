# vsched2 集成 StarryOS — 问题跟踪

## 分支状态

| 分支 | 状态 | 说明 |
|------|------|------|
| **dev** | 🟡 当前工作分支 | P0 LazyInit panic 已修复，存在用户任务内核态 yield 路由错误 |
| **main** | 🟢 可以跑通 | 纯 legacy axtask 调度器 |

## 当前进度

| 项目 | 状态 |
|------|------|
| 初始化流程 | ✅ |
| "Welcome to Starry OS!" 输出 | ✅ |
| vsched2 远端更新适配 | ✅ from_base 删除、activate_vsched_trap_vector 精简、user_init vspace、sources 偏移量 |
| **P1 — free_stacks UAF 导致 base 被清零** | ✅ |
| **P0a — LazyInit\<AxRunQueue\> panic** | ✅ |
| **P0b — 用户任务内核态 yield 路由到 krun_utask** | ❌ **当前阻塞** |

---

## 🔴 P0b — 用户任务内核态 yield 路由到 krun_utask *(当前阻塞)*

### 问题描述

busybox 的 `wait4` → `block_on` → `yield_now` 进入 vsched2 调度后：
- vsched2 的 `ktask_schedule` 按 `pid != 0` 将用户任务路由到 `krun_utask`
- 但此时用户任务正在内核态（执行 syscall），保存的 sepc/sp/tp 均为内核地址
- `krun_utask` → `into_user_context` → 用内核地址 sret 到 U 模式 → 崩溃

### 测试证据

```
[into_user] pid=1 sepc=0xffffffc080380036  ← 内核地址被用作用户态 sret 目标
```

### 修复方向

1. 修改 `ktask_schedule` 判断逻辑——不按 pid==0，按当前上下文判断走 run_task 还是 krun_utask
2. 绕开 block_on 中的 yield（不用 vsched2 调度路径让出 CPU）
3. 修改 vsched2 协程模型

---

## ✅ P1 — free_stacks Vec 重复导致 UAF，base 被清零 *(已修复)*

### 根因

vsched2 的 `free_stacks` 使用 `heapless::Vec`，允许同一个 VSI 引用被 push 多次。
当 `dealloc_stack` 调用 `stack.dealloc()` 释放内存后，Vec 中仍有该 VSI 的重复条目。
下一次 `alloc_stack` 从 Vec 中取出的就是已释放的 VSI → UAF → base 被分配器元数据清零。

### 修复

| 文件 | 改动 |
|------|------|
| `vsched/vsched2/src/stack/handler.rs` | `free_stacks`: `Vec<&'static mut StackVirtImpl, N>` → `FnvIndexMap<usize, &'static mut StackVirtImpl, N>`（key = VSI 地址，唯一去重） |
| `core/src/vsched/stack.rs` | `base()` 改为 `assert!` 防御（magic 损坏 / base 为 null 时立即 abort） |

### 验证

诊断日志确认 UAF 发生后又消失。修复后 `base()` 不再返回 0，系统不再出现 sp=0 崩溃。

---

## ✅ P0a — LazyInit\<AxRunQueue\> panic *(已修复)*

### 根因

`yield_now()` 中 `let f: fn() -> ! = ...; f();` 声明为永不返回，编译器优化后
把 else 分支放在 `call f` 之后。`call` 时 `ra = else 分支地址`，被 trampoline
保存为 `sepc`。任务恢复时跳转到 else 分支 → `current_run_queue()` → LazyInit panic。

### 修复

| 文件 | 改动 |
|------|------|
| `arceos/modules/axtask/src/api.rs` | `yield_now()`: 去掉 `fn() -> !` 语义，用 `asm!("jalr {f}")` + `return` 替代 |

汇编效果：`jalr` 时 `ra = return 之后的地址`，恢复后执行 `ret` 正常返回 block_on 调用点。

---

## 🔧 已实施改动

### P1 修复

| 文件 | 改动 |
|------|------|
| `vsched2/src/stack/handler.rs` | `free_stacks`: `Vec` → `FnvIndexMap<usize, &'static mut StackVirtImpl, N>` |
| `core/src/vsched/stack.rs` | `base()`: assert 防御 + 清理诊断计数器 |

### P0a 修复

| 文件 | 改动 |
|------|------|
| `arceos/modules/axtask/src/api.rs` | `yield_now()`: `fn() -> !` call → `asm!("jalr {f}")` + `return` |

### vsched2 远端更新适配

| 文件 | 改动 |
|------|------|
| `vsched2/src/schedule/scheduler.rs` | sources 偏移量化（`(*const (), ...)` → `(usize, ...)`） |
| `vsched2/src/api.rs` | `user_init()` 改为 `user_init(vspace: *mut ())` |
| `core/src/vsched/stack.rs` | 删除 `from_base` 方法（trait 已删除） |
| `core/src/vsched/trap_vector.rs` | 清理 `SAVED_SSCRATCH`、`old_sscratch` 参数 |
| `core/src/vsched/mod.rs` | 精简 `activate_vsched_trap_vector`（不再分配 raw buffer） |

### 保留的比赛/基础设施改动

- Makefile `all` target → kernel-rv + kernel-la
- init.sh 比赛测试框架
- main.rs SBI shutdown + USE_VSCHED2 开关
- execve/clone/exit vsched2_active 守卫
- axtask/api.rs on_timer_tick 修复

### 调试清理

- 删除 `[ecall]`、`[dispatch]`、`[handler#N]`、`[trap#N]`、`[new_handler]`、`[boot]` 系列调试日志
- `stack.rs` 诊断计数器（STACK_SEQ、LAST_DEALLOC_ADDR 等）已清除

---

## ⚠️ 已知遗留问题

| 问题 | 状态 |
|------|------|
| vsched2 二进制 `log` feature 未启用（无法看到调度日志） | ⚠️ 待处理 |
| execve `process_init` 分配新 pid 导致旧 slot 泄漏 | ⚠️ 待处理 |
| exit 缺少 `process_drop` → PROCESS_INFO 表泄漏 | ⚠️ 待处理 |
| vDSO 物理页在 `uspace.clear()` 后未释放 | ⚠️ 待处理 |
| `do_exit` 中 `clear_child_tid` 继承自父进程 | ⚠️ 待处理 |
| main 分支 vdso_helper 需要更新 feature | ⚠️ 待处理 |
