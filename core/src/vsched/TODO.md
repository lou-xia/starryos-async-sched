# vsched2 集成 StarryOS — 问题跟踪

## 分支状态

| 分支 | 状态 | 说明 |
|------|------|------|
| **dev** | 🟡 当前工作分支 | 所有 vsched2 修复已提交（commits 41f2ec6, 295c63d, 2ae96f8） |
| **main** | 🟢 可以跑通 | 纯 legacy axtask 调度器；当前因 vdso_helper 上游更新缺少 feature |

## 当前进度

| 项目 | 状态 |
|------|------|
| 初始化流程 | ✅ |
| "Welcome to Starry OS!" 输出 | ✅ |
| handle_page_fault n==0 时 PTE 检查 | ✅ |
| ktask_schedule !pop 时同步 priority | ✅ |
| AddrSpace.vdso_base per-process | ✅ |
| clone 用 map_so 重建 vDSO | ✅ |
| vDSO reserved gap 补齐 (mmap 防护) | ✅ |
| musl `__ofl_head` 毒指针 (0xEB230) | ✅ |
| execve `process_reinit` 复用 pid | ✅ |
| yield trampoline sepc/sstatus/SPP 补全 | ✅ |
| 全 syscall save/restore CURRENT_TASK | ✅ |
| exit_group 标记 VschedTaskImpl Exited | ✅ |
| LazyInit<AxRunQueue> panic 根除 | ✅ `init_run_queue_empty` + 4 条 guard |
| do_exit 中 axtask::yield_now 守卫 | ✅ (partially, 见 P1) |
| **pid=1 wait4 死循环** | ❌ **当前阻塞** |

---

## 🔴 P0 — pid=1 wait4 死循环 *(当前阻塞)*

### 问题演进

**阶段一**（已修复）：do_exit 中 yield_now 导致 trap_handler 重启丢失状态
- `trap_handler` loop 中 `pop_front` 后 `handle` → do_exit → yield_now → handler 协程让出
- 重入 `poll()` → `trap_handler` 从头执行 → queue 空 → handler 永久 Blocked
- `process.exit()` 和 `child_exit_event.wake()` 永远不会执行
- **修复**：在 `api/src/task.rs:231` 加 `if !vsched2_active()` 守卫（8aa3b9f）

**阶段二**（新发现）：block_on 中 yield_now 用内核寄存器恢复用户任务
- 修复 do_exit 的 yield 后，hidden 暴露了新问题
- 日志显示：busybox 正常 clone pid=2 → 正常 wait4 → block_on → yield
- 但 yield 的是**用户任务**（pid=1），而非 handler 协程
- 用户任务在 yield 时处于**内核态**（dispatcher 中），trap frame 保存了内核 sepc/sp/tp
- `krun_utask` → `into_user_context` 用内核寄存器进入 U 模式 → scause=4 misaligned access → 死循环

### 根因链

```
dispatcher 设置 CURRENT_TASK = pid=1 (用户任务)
  └─ handle_syscall → sys_waitpid → block_on(vsched2 path)
       └─ poll Pending → woke=false → yield_now()
            └─ VSCHED2_YIELD → yield trampoline 保存 pid=1 内核态寄存器
                 sepc = ra (block_on 内核代码地址)
                 sp = 内核栈
                 tp = 内核 tp
            └─ kschedule → push_prev_task(pid=1 Ready → pid=1 调度器)
                 → process_schedule → 选中最高的 → 再选 pid=1
            └─ ktask_schedule(1) → pid!=0 → return 1 → krun_utask
                 → into_user_context → restore_and_sret(内核寄存器!)
                 → scause=4 crash → 死循环
```

### 核心矛盾

协程通过 `poll()` 重入，**不保存和恢复中间状态**。`trap_handler` 始终从 `loop { pop_front }` 开始。这在 design 上对 handle 中途 yield 不友好。

但用户的根本问题是：**yield_now 错误地 yield 了用户任务**。用户任务在 kernel mode 下 yield 后，被 ktask_schedule 按 `pid != 0` 判定为「用户任务」，强行走 `krun_utask` → `into_user_context` 路径。实际上它应该走 `run_task` → `restore_context`（在 S 模式恢复）。

### 修复方向

1. **修改 ktask_schedule 判断逻辑**（用户提议）：不按 `pid==0` 判断，按 task 的保存上下文判断走 `run_task` 还是 `krun_utask`。需要 vsched2 改动。

2. **绕开 block_on 中的 yield**：用可重启 syscall 或内联 resched 方式。都面临同样矛盾。

3. **根本修复 vsched2 协程模型**：让 trap_handler 记住 yield 点，重入时继续而非重启。改动很大。

当前无结论，待进一步分析。

---

## 🔧 待实施 — vsched2 开关

### 目标

在 `main.rs` 中增加 `const USE_VSCHED2: bool`，一键切换 legacy axtask 和 vsched2。

### 现状分析

当前 dev 分支**硬编码**为 vsched2-only。以下三处需要修改：

#### 1. `src/main.rs` — 入口分支

```rust
// 当前（dev）:
fn main() {
    ...
    let (init_ptr, vspace_ptr) = create_vsched_init_task(&args, &envs);  // vsched2 专用
    starry_core::vsched::vsched2_bootstrap(Some(init_ptr as *const ()), Some(vspace_ptr));
}

// 开关版本:
const USE_VSCHED2: bool = true;  // ← 一键切换

fn main() {
    starry_api::init();
    vdso::vdso_init();
    let args = ...;

    if USE_VSCHED2 {
        let (init_ptr, vspace_ptr) = create_vsched_init_task(&args, &envs);
        starry_core::vsched::vsched2_bootstrap(Some(init_ptr as *const ()), Some(vspace_ptr));
    } else {
        entry::run_initproc(&args, &envs);
    }
}
```

当 `USE_VSCHED2=false` 时 `create_vsched_init_task` 不会被调用，编译器消除其代码。需加 `#[allow(unused_imports)]` 抑制 vsched2 相关 import 警告。

#### 2. `arceos/modules/axtask/src/api.rs` — 恢复 `on_timer_tick`

```rust
// main 分支（正确完整版）:
#[cfg(feature = "irq")]
pub fn on_timer_tick() {
    crate::timers::check_events();               // ← 丢了这个
    current_run_queue::<NoOp>().scheduler_timer_tick();
}

// dev 分支（当前）:
pub fn on_timer_tick() {                         // ← 丢了 #[cfg(feature = "irq")]
    if vsched2_active() {                        // ← vsched2 守卫（正确）
        return;
    }
    current_run_queue::<NoOp>().scheduler_timer_tick();  // ← 丢了 check_events()
}

// 开关版本:
#[cfg(feature = "irq")]
pub fn on_timer_tick() {
    if vsched2_active() {
        return;                                  // vsched2 自己处理定时器
    }
    crate::timers::check_events();               // legacy 路径恢复
    current_run_queue::<NoOp>().scheduler_timer_tick();
}
```

**改动点**：恢复 `#[cfg(feature = "irq")]` 和 `crate::timers::check_events()`。`vsched2_active()` 守卫保留（vsched2 活跃时跳过 legacy 路径）。

#### 3. 其他地方 — 无需修改

dev 分支的所有 vsched2 相关改动已通过运行时 guard 条件化：

| 文件 | guard | 说明 |
|------|-------|------|
| `axtask/src/api.rs:136` | `if vsched2_active() { return; }` | on_timer_tick |
| `axtask/src/api.rs:278` | `VSCHED2_YIELD != 0` | yield_now 委托 vsched2 |
| `axtask/src/task.rs:397` | `if vsched2_active()` | current_check_preempt_pending |
| `axtask/src/future/mod.rs:50` | `if !vsched2_active()` | AxWaker unblock_task |
| `axtask/src/future/mod.rs:64` | `if vsched2_active()` | block_on 路径选择 |
| `axtask/src/run_queue.rs:669` | `init_empty()` | 惰性初始化空队列 |

这些 guard 在 USE_VSCHED2=false 时自动走 legacy 路径，无需额外改动。

### 开关语义

```
USE_VSCHED2 = true:
  main.rs  → create_vsched_init_task() + vsched2_bootstrap()
  api.rs   → vsched2_active() = true → 跳过 legacy timer
  axtask   → VSCHED2_YIELD != 0 → yield 委托 vsched2

USE_VSCHED2 = false:
  main.rs  → entry::run_initproc()
  api.rs   → vsched2_active() = false → timers::check_events() + scheduler_timer_tick()
  axtask   → VSCHED2_YIELD == 0 → 走原始 AxRunQueue
```

### 改动总结

| # | 文件 | 行数 | 改动 |
|---|------|------|------|
| 1 | `src/main.rs` | ~5 | 加 `const USE_VSCHED2: bool` + if/else 入口分支 + allow unused_imports |
| 2 | `arceos/modules/axtask/src/api.rs` | ~3 | restore `#[cfg(feature = "irq")]` + `crate::timers::check_events()` |

总计约 8 行改动，不改 vsched2 crate，不回退已有修复。

---

## ⚠️ 已知遗留问题

| 问题 | 状态 |
|------|------|
| execve `process_init` 分配新 pid 导致旧 slot 泄漏 | ⚠️ 待处理 |
| exit 缺少 `process_drop` → PROCESS_INFO 表泄漏 | ⚠️ 待处理 |
| vDSO 物理页在 `uspace.clear()` 后未释放 | ⚠️ 待处理 |
| `do_exit` 中 `clear_child_tid` 继承自父进程 | ⚠️ 待处理 |
| main 分支 vdso_helper 需要更新 feature | ⚠️ 待处理 |
