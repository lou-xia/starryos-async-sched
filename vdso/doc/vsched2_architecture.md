# vsched2 调度架构与 StarryOS 集成备忘

> 本文档记录了 vsched2 的完整调度架构，包括入口点、调度循环、任务切换模型、
> 以及 StarryOS 集成时需要提供的 OS 侧职责。供后续开发和上下文恢复时快速上手。

---

## 目录

1. [模块定位](#1-模块定位)
2. [入口点体系](#2-入口点体系)
3. [任务切换模型（三步法）](#3-任务切换模型三步法)
4. [调度循环详解](#4-调度循环详解)
5. [栈管理体系](#5-栈管理体系)
6. [地址空间与进程管理](#6-地址空间与进程管理)
7. [寄存器约定（RISC-V）](#7-寄存器约定risc-v)
8. [OS 必须提供的接口](#8-os-必须提供的接口)
9. [OS 必须完成的汇编级职责](#9-os-必须完成的汇编级职责)
10. [StarryOS 集成现状与差距](#10-starryos-集成现状与差距)
11. [关键源码路径](#11-关键源码路径)

---

## 1. 模块定位

vsched2 的职责是提供一套"**统一调度框架**"，**不直接创建任务或管理内核对象**。

- **外部（OS）负责**：定义真实任务对象、保存/恢复任务上下文、提供栈分配与回收、提供地址空间切换、提供 trap 处理任务、在合适的时机把控制权交给调度器
- **vsched2 内部负责**：维护调度循环、维护就绪队列与事件源、在特权级之间切换调度逻辑、基于优先级选择下一任务、维护共享状态

vsched2 编译为 `.so`（vDSO），通过 `build_vdso` 工具生成 `libvsched2` Rust 包装库。

---

## 2. 入口点体系

vsched2 通过**三个汇编入口**接收外部控制流。所有入口都在 `schedule_loop`（汇编函数）中。

### 2.1 `raw_trap_entry` — 中断/异常/ecall 入口

```
调用方: OS trap 向量（完成上下文保存后）
参数: a0 = trap_type (0=同步trap, 1=外部中断, 2=用户调度器主动陷入)
      a1 = privilege (0=内核态, 1=用户态)
行为: 保存 a1→s1, s2=0 → 调 trap_entry() → 根据返回值分发
```

**返回值分发**：

| 返回值 | 跳转目标 | 含义 |
|--------|---------|------|
| 0 | `raw_trap_handle` | 同步 trap，需要 trap 处理 |
| 1 | `raw_kschedule` | 进入内核调度 |
| 2 | `raw_uschedule` | 进入用户调度 |
| 3 | `raw_utok_schedule` | 进入"用户调度器刚陷入内核"路径 |

### 2.2 `raw_thread_entry` — 线程主动让权入口

```
调用方: OS 线程上下文保存完成之后
参数: 无（通过 s1/s2 传递状态）
行为: s2=1 → 调 thread_entry() → 返回值存 s1 → 分发
```

### 2.3 `raw_run_task` — 任务执行入口（内部循环入口）

```
调用方: 调度器选好任务后
参数: s1=privilege, s2=stack_status
行为: 调 run_task(privilege, stack_status)
```

**协程从此返回**：`raw_run_task` 是协程 `run_coroutine()` 返回后的汇合点。根据返回值分发到 `raw_kschedule` 或 `raw_uschedule`。

### 2.4 `raw_kschedule` — 内核初始化入口

```
调用方: OS 初始化代码
前置条件: s1=0(内核态), s2=0(空栈)
行为: 进入内核调度循环
```

---

## 3. 任务切换模型（三步法）

（来源：`任务切换模型（新版调度算法）.drawio`）

### 步骤1：执行流保存

| 入口 | 保存内容 | 谁完成 |
|------|---------|--------|
| trap_entry（内核态） | 切换到预保存栈、保存必要寄存器到 trap 上下文 | **OS（纯汇编）** |
| trap_entry（用户态） | 同上 | **OS（纯汇编）** |
| thread_entry | 保存线程上下文（TaskContext） | **OS（纯汇编）** |
| run_task（协程） | 从 `poll()` 返回、根据返回值修改任务状态 | vsched2 内部 |

### 步骤2：特权级/地址空间切换 + 任务调度

1. **状态检查**：如果上一个是就绪态 → 放回调度器（`push_prev_task`）
2. **优先级更新**：`process_schedule()` → `PROCESS_INFO_TABLE`
3. **进程选择**：`highest_prio_process()` 在全局进程表中选最高优先级进程
4. **任务取出**：`scheduler.pop_task()` 从目标进程调度器中取最高优先级任务
5. **地址空间切换**：跨进程时调用 `VSpace::into_vspace()`

**异常**：如果在内核态运行用户态任务，则特权级切换延后到步骤3——这主要是为了使内核运行用户线程时可以同时恢复上下文和切换地址空间。

### 步骤3：执行流恢复

| 任务类型 | 栈类型 | 行为 |
|---------|--------|------|
| 线程（内核态→内核态） | 非空栈 | `save_thread_context()` → 切换栈 → `restore_context()` |
| 线程（内核态→用户态） | 非空栈 | 需要先进入用户调度器再恢复 |
| 协程（内核态） | 空栈 | `poll()` → 返回到调度循环 |
| 协程（用户态） | 空栈 | 需要先通过用户调度器进入 |

**异常**：如果在内核态运行用户态任务，则特权级切换在**本步骤**进行（从步骤2延后）。

---

## 4. 调度循环详解

vsched2 使用**函数跳转链**而非递归调用——函数间通过 `reset_stack_and_jump!` 直接跳转。

### 4.1 核心 Rust 函数

| 函数 | 职责 | 返回值 |
|------|------|--------|
| `trap_entry(trap_type, privilege)` | 分配新预保存栈，决定下一步路径 | 0=trap_handle, 1=kschedule, 2=uschedule, 3=utok_schedule |
| `trap_handle()` | 获取 trap 处理任务，设为当前任务 | — |
| `kschedule()` | 内核态调度：放回旧任务→选进程→选任务→跑任务 | 0=run_task, 1=krun_utask |
| `uschedule(stack_status)` | 用户态调度 | 只有同进程时才返回 |
| `utok_schedule()` | "用户→内核"后的调度 | 0=run_task, 1=krun_utask |
| `run_task(privilege, stack_status)` | 核心执行分发：协程→`run_coroutine()`，线程→`run_thread()` | 仅协程返回 |
| `krun_utask(stack_status)` | 内核态运行用户态任务 | **永不返回** |
| `run_coroutine()` | 调用 `task.poll()`，处理结果 | 0=内核, 1=用户 |
| `run_thread()` | 调用 `task.restore_context()` | **永不返回** |
| `run_coroutine_into_user(user_sp)` | 切换到用户协程 | **永不返回** |
| `run_thread_into_user()` | 切换到用户线程 | **永不返回** |

### 4.2 跨特权级回环（黄色箭头）

当用户态调度器发现需要切换到其他进程（当前 < 全局最高优先级）：

1. `uschedule()` → `utask_schedule()` 
2. `CURRENT_VSPACE` 设为目标进程号
3. 回收用户调度器的栈
4. 调用 `Context::into_kernel()` —— 一个特殊的 ecall（系统调用号=特定值）
5. 内核 trap 向量识别这个特殊 ecall → **跳过上下文保存和状态修改**
6. 分发到 `utok_schedule()` → 在目标进程调度器中选任务 → `krun_utask()`

**注意**：这个特殊 ecall 会**两次进入调度代码**。第一次进入时已经在用户调度器内部完成了调度决策。
图中黄色箭头（`#fff2cc`/`#d6b656`）标注了仅在第二次进入时经过的流程。

---

## 5. 栈管理体系

### 5.1 栈类型

| 类型 | 用途 | 何时切换 |
|------|------|---------|
| **空栈** | 调度循环自身、协程执行、trap 处理 | 进入调度器/trap 处理时分配 |
| **非空栈（线程栈）** | 线程执行 | 恢复线程上下文前切换 |

### 5.2 核心结构

| 结构 | 位置 | 用途 |
|------|------|------|
| `StackWapper` | `stack.rs` | 栈对象封装（栈底地址） |
| `StackHandler` | `stack.rs` | 栈池（空闲池 + 每个 CPU 的当前栈） |
| `KERNEL_STACKS` | `current.rs` | 内核栈池（共享） |
| `STACK_HANDLER` | `current.rs` | 进程私有栈池（通过 `get_user_data` 访问） |

### 5.3 关键方法

| 方法 | 行为 |
|------|------|
| `StackHandler::new()` | 预分配 `STACK_POOL_SIZE - CPU_NUM` 个空闲栈 + CPU_NUM 个当前栈 |
| `alloc_stack()` | 从空闲池取，空则新分配 |
| `dealloc_stack()` | 放回空闲池，满则释放 |
| `get_empty_stack(stack_status)` | 获取空栈（协程用），status≠0 时回收旧栈 |
| `get_thread_stack(thread_stack, stack_status)` | 切换到线程栈 |

### 5.4 跳板（Trampoline）

| 跳板 | 作用 |
|------|------|
| `coroutine_trampoline()` | `mv sp, a0; j run_coroutine` — 切换 sp 后执行协程 |
| `thread_trampoline()` | `mv sp, a0; j run_thread` — 切换 sp 后执行线程 |

---

## 6. 地址空间与进程管理

### 6.1 进程表

```
PROCESS_INFO_TABLE (ProcessInfoTable)
  ├─ table[i].highest_prio  // 进程 i 在所有事件源中的最高优先级
  ├─ table[i].vspace        // 进程 i 的地址空间指针 (*mut ())
  └─ table[i].valid         // 进程号是否有效（0号固定保留给内核）
```

### 6.2 状态变量

| 变量 | 类型 | 含义 |
|------|------|------|
| `CURRENT_VSPACE[cpu_id]` | `AtomicUsize` | 当前地址空间所属进程号 |
| `IN_KERNEL[cpu_id]` | `AtomicBool` | 是否在内核态 |
| `CURRENT_TASK[cpu_id]` | `AtomicPtr<()>` | 当前运行的任务指针 |
| `KERNEL_SCHEDULER` | `LazyInit<AtomicPtr<Scheduler>>` | 内核调度器实例 |

### 6.3 地址空间切换

```
switch_vspace(vspace_pid):
  if CURRENT_VSPACE != target_pid:
    vspace = PROCESS_INFO_TABLE[target_pid].vspace
    VSpace::into_vspace(vspace)  // 写 SATP
```

---

## 7. 寄存器约定（RISC-V）

### 7.1 调度循环中的持久寄存器

| 寄存器 | 含义 |
|--------|------|
| `s1` | 当前特权级：`0`=内核态, `1`=用户态 |
| `s2` | 调度循环栈状态：`0`=空栈, `1`=非空栈 |

这两个 callee-saved 寄存器在整个调度循环的跳转链中保持不变（因为栈切换会使正常函数 prologue/epilogue 失效）。

### 7.2 汇编宏

| 宏 | 行为 |
|----|------|
| `reset_stack_and_jump!(fn)` | `sp=fp; l{w,d} ra,-XLEN(fp); l{w,d} fp,-2*XLEN(fp); j fn` — 重置栈帧并跳转 |
| `switch_sp_tratrampoline!(fn)` | `mv sp, a0; j fn` — 裸函数，切换栈 |
| `jump_to_trampoline!(fn, new_sp)` | `l{w,d} ra,-XLEN(fp);` 然后跳转到跳板传递 new_sp |

### 7.3 栈帧布局

```
高地址 (fp)
  +0:     (空)
  -XLEN:  保存的 ra
  -2*XLEN: 保存的 fp
  ... :   其他保存的寄存器
  ... :   局部变量
低地址 (sp)
```

---

## 8. OS 必须提供的接口

（即 `trait_interface!` 声明的 trait，在 `vsched2/src/interface.rs` 定义）

### 8.1 Task 接口

```rust
fn state(&self) -> TaskState;                 // 0=Ready, 1=Running, 2=Blocked, 3=Exited
fn set_state(&self, state: TaskState) -> TaskState;  // 返回旧状态
fn priority(&self) -> isize;                  // 越小越高，范围 0..=15
fn is_coroutine(&self) -> bool;               // true=协程, false=线程
fn pid(&self) -> usize;                       // PROCESS_INFO_TABLE 索引，0=内核
fn set_pid(&self, pid: usize);                // trap 处理任务继承用
fn save_thread_context(&self);                // 保存线程上下文
fn save_trap_context(&self);                  // 保存 trap 上下文（预留）
fn restore_context(&self);                    // 恢复线程上下文（不返回）
fn poll(&self) -> Poll<usize>;                // 协程轮询
fn thread_stack_base(&self) -> usize;         // 线程栈底地址
fn set_return_value(&self, value: usize);     // 记录协程返回值
```

### 8.2 其余接口

| Trait | 方法 | 说明 |
|-------|------|------|
| `Stack` | `alloc() -> *mut ()`, `dealloc(stack)` | 栈分配/回收 |
| `Context` | `into_kernel() -> !`, `into_user(ustack)`, `into_user_context(task)`, `switch_vspace(vspace)` | 特权级和地址空间切换 |
| `TrapHandle` | `get_handler(task) -> *const ()` | 获取 trap 处理任务 |
| `SMP` | `cpu_id() -> usize` | 获取 CPU ID |
| `VSpace` | `into_vspace(vspace: *mut ())` | 切换地址空间（调度主路径） |
| `UserData` | `get_user_data(pos, len) -> *mut ()` | 内核→用户 vVAR 地址翻译 |

---

## 9. OS 必须完成的汇编级职责

（从 drawio 图中"约定OS完成"标注）

### 9.1 内核态 trap 向量（纯汇编）

```
流程:
  1. 读取 sscratch → 获得预保存栈地址
  2. 切换到该栈（mv sp, sscratch_value）
  3. 保存必要寄存器（callee-saved + 上下文相关寄存器）
  4. 设置 a0 = trap_type, a1 = privilege
  5. 跳转到 raw_trap_entry
```

### 9.2 用户态 trap 向量（纯汇编）

```
流程: 同内核态，但从用户态进入（stvec 指向此向量）
```

### 9.3 线程上下文保存（纯汇编）

```
流程:
  1. 线程主动让权时调用
  2. 保存完整 TaskContext（ra, sp, s0-s11, 等）
  3. 跳转到 raw_thread_entry
```

### 9.4 预保存栈管理

```
sscratch ← 预分配的内核栈地址
每个 CPU 需要一个独立的 sscratch 栈
trap 返回前需要重新分配 sscratch 栈
```

---

## 10. StarryOS 集成现状与差距

### 10.1 已完成

| 项目 | 文件 |
|------|------|
| 7 个 trait 接口注册 | `core/src/vsched.rs` |
| Task 状态映射（axtask ↔ vsched2） | 同上 |
| Stack 分配/回收（全局分配器） | 同上 |
| SMP（this_cpu_id） | 同上 |
| VSpace（SATP 操作） | 同上 |
| UserData（页表遍历地址翻译） | 同上 |
| TrapHandle（全局工厂模式） | 同上 |
| `libvsched2` 依赖引入 | `core/Cargo.toml` |
| vDSO 加载和元数据占位 | `core/src/vsched.rs:init_vsched2_interfaces()` |

### 10.2 待实现（panic 占位）

| 方法 | 位置 | 需要实现 |
|------|------|---------|
| `StarTask::restore_context()` | `core/src/vsched.rs:168` | 调用 `TaskContext::switch_to()` 恢复线程上下文 |
| `StarryContext::into_kernel()` | `core/src/vsched.rs:264` | 特殊 ecall 进入内核调度 |
| `StarryContext::into_user()` | `core/src/vsched.rs:268` | sret 进入用户协程 |
| `StarryContext::into_user_context()` | `core/src/vsched.rs:272` | sret 进入用户线程 |

### 10.3 待实现（汇编入口层）

| 项目 | 说明 |
|------|------|
| **内核态 trap 向量** | 需要在现有 StarryOS trap 向量中增加"识别 vsched2 特殊 ecall"和"跳转到 raw_trap_entry"的逻辑 |
| **预保存栈（sscratch）** | 需要预分配内核栈并写入 sscratch CSR |
| **线程上下文保存** | 线程让权时需要保存 TaskContext 并调用 `raw_thread_entry` |
| **调度循环启动** | OS boot 时调用 `raw_kschedule` 进入 vsched2 调度循环 |

### 10.4 两套调度器并存问题

当前 StarryOS 使用 axtask 的调度循环（`resched()` → `switch_to()`），
vsched2 有自己的调度循环（`kschedule`/`uschedule` → `run_task`）。

需要在同一内核中共存：
- 同一个 trap 向量 → 通过 trap_type 和 privilege 参数分流
- 同一个 CPU 核 → 并发控制
- 同一套地址空间管理

### 10.5 vDSO 元数据占位

`VSCHED2_VVAR_START_PA` 等变量当前为零占位值，需要从 `libvsched2::load_and_init()` 获取实际映射地址。

---

## 11. 关键源码路径

### vsched2 核心

| 文件 | 内容 |
|------|------|
| `vsched/vsched2/src/main_loop.rs` | 调度主循环（trap_entry, kschedule, uschedule, run_task, ...） |
| `vsched/vsched2/src/arch/riscv.rs` | RISC-V 汇编入口和宏 |
| `vsched/vsched2/src/current.rs` | 全局状态（CURRENT_TASK, KERNEL_SCHEDULER, ...） |
| `vsched/vsched2/src/stack.rs` | 栈管理（StackHandler, StackWapper, 跳板） |
| `vsched/vsched2/src/interface.rs` | trait_interface 声明（Task, Stack, Context, ...） |
| `vsched/vsched2/src/schedule/scheduler.rs` | 调度器（Scheduler, 事件源管理） |
| `vsched/vsched2/src/schedule/ready_queue.rs` | 就绪队列（分优先级 FIFO） |
| `vsched/vsched2/src/schedule/process_info.rs` | 进程信息表 |
| `vsched/vsched2/src/api.rs` | **空白**（公共 API 占位） |
| `vsched/vsched2/build.rs` | 编译期配置（CPU_NUM, PRIORITY, ...） |

### StarryOS 集成

| 文件 | 内容 |
|------|------|
| `core/src/vsched.rs` | vsched2 接口适配（7 个 trait 实现 + 初始化） |
| `core/src/lib.rs` | `extern crate libvsched2` 强制链接 |
| `core/Cargo.toml` | `libvsched2` 路径依赖 |
| `vdso/build.rs` | `build_vdso` 调用 → 生成 `vdso_vsched2_output/libvsched2/` |
| `vdso_vsched2_output/libvsched2/` | 自动生成的包装库（load_and_init, init_vtable_*, 加载器） |
| `vsched/vsched2/mydocs/接口文档.md` | vsched2 完整接口规范文档 |
| `vdso/doc/vsched2_interfaces.md` | StarryOS 集成设计文档 |
