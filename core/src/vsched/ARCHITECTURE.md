# StarryOS vsched2 集成架构文档

## 项目概述

将 vsched2 统一调度器（基于 vDSO 机制）移植到 StarryOS。vsched2 是一个分层固定优先级调度器，支持内核/用户协同、线程/协程统一抽象、多事件源扩展。

## vsched2 核心设计

### 调度入口
```
raw_trap_entry ──→ trap_entry ──→ 分发:
                    ├─ trap_type=0 (同步) → push_trap → kschedule
                    ├─ trap_type=1 (中断) → push_trap → kschedule
                    └─ trap_type=2 (特殊ecall) → utok_schedule

raw_thread_entry ──→ thread_entry ──→ kschedule / uschedule
```

### 调度大循环 (kschedule)
1. `push_prev_task`: 将当前任务按状态放回就绪队列
2. `process_schedule`: 全局进程优先级比较, 选择目标进程
3. `ktask_schedule`: 从目标调度器取出任务
   - pid=0: 从内核调度器取 → `run_task`
   - pid!=0: 切换到用户地址空间 → 从用户调度器取 → `krun_utask`
4. `run_task`: 协程走 `poll()`, 线程走 `restore_context()`

### 事件源 (Event Source)
调度器包含多个事件源, `pop_task()` 在事件源之间按优先级选择:
- **ReadyQueue**: 默认就绪队列, 分优先级 FIFO
- **TrapWaitQueue**: trap 处理队列, 存 `(TrapInfo, Option<task>)`

当 trap 队列非空时, `take_task` 返回 trap handler 协程。

### trap 处理 (最新设计 - 26.6.1)
ALL traps 统一走事件源:
1. trap 进入 → `trap_entry` → 设置任务状态（同步=Blocked, 中断=Ready）
2. 创建 TrapInfo + 可选择关联任务 → push 到 trap 队列
3. 进入调度器 → 从事件源选择最高优先级 → 通常选中 trap handler
4. trap handler 调用 `TrapInfo::handle` → 分析 scause → 处理（syscall/缺页/信号）
5. 处理完后: `task.set_state(Ready)` → `scheduler.push_task(task)` → 唤醒任务

### 任务运行
- **协程**: `run_task` → `run_coroutine` → `poll()` → 返回 Poll::Ready/Pending
- **线程(内核)**: `run_task` → `thread_trampoline` → `run_thread` → `restore_context()` → `restore_and_jump(Yield)` → `ret`
- **线程(用户)**: `krun_utask` → `run_thread_into_user` → `into_user_context` → `restore_and_sret`

## StarryOS 集成要点

### 关键文件
| 文件 | 职责 |
|---|---|
| `core/src/vsched/trap_vector.rs` | trap 向量: SATP切换, gp恢复, 上下文保存, stub调用, yield trampoline |
| `core/src/vsched/trapframe.rs` | UserTrapFrame 结构及 restore_and_sret_user / restore_and_jump |
| `core/src/vsched/task.rs` | VschedTaskImpl: Task trait 实现, restore_context |
| `core/src/vsched/context.rs` | VschedContextImpl: into_kernel/into_user/into_user_context; VschedVSpaceImpl: into_vspace |
| `core/src/vsched/mod.rs` | bootstrap, trap vector 激活, 接口注册 |
| `core/src/vsched/trap.rs` | VschedTrapInfoImpl: TrapInfo trait 实现, trap handler 协程 |
| `core/src/vsched/userdata.rs` | vVAR/vDSO 用户态地址翻译 |
| `api/src/task.rs` | vsched_trap_dispatcher: scause 分发(syscall/缺页/信号) |
| `src/main.rs` | 用户任务创建, vDSO 基址设置, bootstrap 调用 |
| `vsched/vsched2/src/main_loop.rs` | trap_entry, kschedule, run_task, krun_utask |
| `vsched/vsched2/src/arch/riscv.rs` | raw_trap_entry, raw_run_task, 汇编入口与跳板 |

### SATP 切换时机
- `into_vspace` (VschedVSpaceImpl): 切换用户页表, 设置 SUM, flush TLB
- `restore_and_sret_user`: 加载全部寄存器 → `csrw satp` → `sfence.vma` → `sret`
  关键: trap frame 在 kernel heap, 必须在 kernel PT 下加载寄存器

### 页表映射
- `copy_from_kernel`: 复制内核映射到用户地址空间
- `copy_mappings_from`: 同步最新内核映射
- `handle_page_fault` in restore_context: 确保用户代码页(0x100b0)已映射

### 已修复的 Bug
1. yield trampoline ra/sp 偏移互换 → 代码损坏
2. bootstrap unreachable!() → 主任务恢复时崩溃
3. trap_wait_queue 事件源顺序 → trap handler 优先
4. set_pre_stack! 注释 → sscratch 不应修改
5. 定时器中断 stimecmp 确认 → 防止无限重入
6. trap dispatcher 中断忽略 → 不杀用户任务
7. restore_and_sret_user SATP 切换 → kernel PT 下加载寄存器
