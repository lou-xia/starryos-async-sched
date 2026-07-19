# StarryOS vsched2 集成架构文档

## 相关文档索引

| 文档 | 路径 | 说明 |
|------|------|------|
| **TODO.md** | `core/src/vsched/TODO.md` | 问题跟踪、已修/待修列表、调试笔记 |
| **block_on 设计简述** | `core/src/vsched/block_on设计简述.md` | 当前问题、短期 SyscallTask 方案与最终协程目标 |
| **block_on 设计文档** | `core/src/vsched/block_on设计文档.md` | 统一 Execution Flow、SyscallExecutionFlow、park/wake、多核与异步 syscall 设计 |
| **vsched2 接口文档** | `vdso/doc/vsched2_interfaces.md` | vDSO VTABLE 接口定义、调用关系 |
| **vsched2 架构文档** | `vdso/doc/vsched2_architecture.md` | vsched2 内部模块架构 |
| **vsched2 调度核心** | `vsched/vsched2/src/main_loop.rs` | kschedule / push_prev_task / run_task / krun_utask |
| **vsched2 进程表** | `vsched/vsched2/src/schedule/process_info.rs` | highest_prio_process / register_process |
| **vsched2 调度器** | `vsched/vsched2/src/schedule/scheduler.rs` | pop_task / push_task / update_prio |
| **vsched2 栈管理** | `vsched/vsched2/src/stack/handler.rs` | free_stacks / alloc_stack / dealloc_stack |
| **vsched2 陷阱队列** | `vsched/vsched2/src/schedule/trap_wait_queue.rs` | hightest_priority / take_task |
| **vDSO 日志 (log_init)** | `vdso_crate_template/.../vdso_helper/src/log_init.rs` | init_log / LogVirtImpl |
| **vDSO 加载器** | `vdso_vsched2_output/libvsched2/src/loader.rs` | map_so（ELF 段映射 + 重定位） |
| **StarryOS VSpace** | `core/src/vsched/context.rs` | into_vspace / into_user_context / CURRENT_VDSO_BASE |
| **StarryOS UserData** | `core/src/vsched/userdata.rs` | get_user_data（5 步地址转换） |
| **StarryOS trap 向量** | `core/src/vsched/trap_vector.rs` | vsched2_trap_vector / vsched_yield_trampoline |
| **StarryOS 栈实现** | `core/src/vsched/stack.rs` | VschedStackImpl（alloc/dealloc/base） |
| **StarryOS trap handler** | `core/src/vsched/trap.rs` | toggle_handler / TrapHandlerCoroutine |
| **StarryOS 任务实现** | `core/src/vsched/task.rs` | VschedTaskImpl / register_task |
| **StarryOS 调度初始化** | `core/src/vsched/mod.rs` | vsched2_bootstrap / TRAPPED_VSCHED_TASK |
| **block_on / yield** | `arceos/modules/axtask/src/future/mod.rs` + `api.rs` | block_on 调度接口 / yield_now / toggle 注册 |
| **Task State 桥接** | `arceos/modules/axtask/src/api.rs` | BLOCK_ON_TOGGLE / register_block_on_toggle |
| **CPU trap vec** | `arceos/modules/axcpu/src/riscv/trap.rs` | 页故障 panic handler |
| **VDSO 链接器脚本** | `vdso_vsched2_output/vdso_linker.lds` | 段布局：text_seg(RX) / ro_seg(R) / data_seg(RW) |
| **VDSO 版本映射** | `vdso_vsched2_output/vdso_version.map` | 导出符号列表 |
| **VDSO VTABLE 偏移表** | `vdso_vsched2_output/libvsched2/src/api.rs` | 各函数在 .so 中的偏移（自动生成） |

## 项目概述

将 vsched2 统一调度器（基于 vDSO 机制）移植到 StarryOS。vsched2 是一个分层固定优先级调度器，支持内核/用户协同、线程/协程统一抽象、多事件源扩展。

## vsched2 核心设计

### 三步调度模型

vsched2 将调度循环分解为三个步骤：
```
步骤1: 执行流保存
   └─ trap_vector (OS汇编): 切换到预保存栈, 保存全部寄存器+CSR, 恢复sscratch
   └─ trap_entry (vsched2): 分配新预保存栈, 设置任务状态, push_trap → kschedule
   └─ thread_entry (vsched2): yield时设置任务状态 → kschedule

步骤2: 特权级与地址空间切换、任务调度
   └─ kschedule / uschedule: push_prev_task → process_schedule → ktask_schedule
      内核态: kschedule → process_schedule → ktask_schedule(0) → run_task
      用户态: uschedule → select&pop → run_task
      陷入内核: utok_schedule → select&pop → krun_utask
   例外: 内核态运行用户任务时, 特权级切换延后到步骤3

步骤3: 执行流恢复
   └─ run_task: 内核线程→restore_context(), 内核协程→poll()
   └─ krun_utask: 内核运行用户任务, 特权级切换在本步骤完成(sret)
      用户线程→into_user_context→restore_and_sret
      用户协程→into_user→sret to raw_run_task
```

### OS 与 vsched2 的职责边界

```
┌─ OS 负责 (纯汇编) ─────────────────────────────┐
│ trap_vector: 保存上下文, 恢复sscratch, SUM/MXR │
│ yield_trampoline: 保存callee-saved寄存器       │
│ restore_and_sret/restore_and_jump: 恢复上下文   │
└────────────────────────────────────────────────┘

┌─ vsched2 负责 (vDSO .so) ──────────────────────┐
│ trap_entry: 分配栈, 设置状态, push_trap         │
│ thread_entry: 设置状态 → kschedule              │
│ kschedule/uschedule/utok_schedule: 任务调度     │
│ run_task/krun_utask: 栈管理, 上下文恢复入口     │
│ trap_handler: 协程处理trap队列                  │
└────────────────────────────────────────────────┘

┌─ StarryOS 中转 ────────────────────────────────┐
│ vsched2_trap_entry_stub: TF_POOL, stimecmp ack  │
│ vsched_yield_entry_stub: heap Box分配           │
│ vsched2_direct_entry_stub: into_kernel快速路径   │
└────────────────────────────────────────────────┘
```

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

### 事件源
调度器包含多个事件源, `pop_task()` 在事件源之间按优先级选择:
- **ReadyQueue**: 默认就绪队列, 分优先级 FIFO
- **TrapWaitQueue**: trap 处理队列, 存 `(TrapInfo, Option<task>)`

当 trap 队列非空时, `take_task` 返回 trap handler 协程。

### trap 处理 (事件源驱动)
ALL traps 统一走事件源:
1. trap 进入 → `trap_entry` → 设置任务状态（同步=Blocked, 中断=Ready）
2. 创建 TrapInfo + 可选择关联任务 → push 到 trap 队列
3. 进入调度器 → 从事件源选择最高优先级 → 通常选中 trap handler
4. trap handler 调用 `TrapInfo::handle` → 分析 scause → 处理（syscall/缺页/信号）
5. 处理完后: `task.set_state(Ready)` → `scheduler.push_task(task)` → 唤醒任务

### 任务运行路径
- **协程(内核)**: `run_task` → `get_empty_stack` → `coroutine_trampoline` → `run_coroutine` → `poll()`
- **线程(内核)**: `run_task` → `get_thread_stack(Some)` → `thread_trampoline` → `run_thread` → `restore_context()` → `restore_and_jump(Yield)` → `ret`
- **线程(用户)**: `krun_utask` → `get_thread_stack(None)` → `run_thread_into_user` → `into_user_context` → `restore_and_sret`
- **协程(用户)**: `krun_utask` → `get_empty_stack(user)` → `get_thread_stack(None)` → `run_coroutine_into_user` → `into_user(ustack)`

### 预保存栈 (sscratch) 约定
- 初始栈: `activate_vsched_trap_vector` 分配 256KB 原始堆缓冲区
- 每次 trap: `trap_entry` 中 `alloc_stack().base()` → `set_pre_stack!` 更新 sscratch
- 旧栈回收: vsched2 预期 `trap_entry` 中将旧 sscratch 栈通过 `set_current_stack` 管理, 在 `run_task`/`krun_utask` 时回收
- StarryOS 当前状态: 旧 sscratch 栈尚未回收 (TODO)

### 初始化序列 (当前实现)

```
1. disable_irqs()
2. init_vsched2_interfaces()         // 注册 7 个 trait + init_raw_run_task_offset
3. register_vsched2_yield()          // 替换 axtask::yield_now → vsched_yield_trampoline
4. kernel_init_main(init_stack, main) // vsched2: 初始化内核调度器(pid=0)
5. copy_mappings_from(kernel→user)   // 内核页面映射到用户 PT
6. write_user_page_table(user_root)  // 切入用户 PT
7. process_init(vspace)              // vsched2: 创建进程调度器(pid=1)
8. push_task_into_process(task, 1)   // vsched2: 推入用户任务
9. write_user_page_table(kernel_root)// 切回内核 PT
10. copy_mappings_from sync           // 同步 process_init 产生的新映射
11. activate_vsched_trap_vector()     // 分配预保存栈, 设置 sscratch, 设置 stvec
12. yield loop                        // call vsched_yield_trampoline 反复
```

## StarryOS 集成关键文件

| 文件 | 职责 |
|---|---|
| `core/src/vsched/trap_vector.rs` | trap 向量 (gp恢复, 上下文保存, stub调用, yield trampoline) |
| `core/src/vsched/trapframe.rs` | UserTrapFrame 及 restore_and_sret / restore_and_jump |
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
- **trap 入口**: 不切 SATP — 内核页面已映射在所有用户页表的高地址区域
- **`into_vspace`**: 切换用户页表 (`write_user_page_table` + `sfence.vma` + SUM)
- **`restore_and_sret`**: 全量恢复寄存器后 `sret`, 用户 PT 已由 `into_vspace` 提前激活
- **`restore_and_sret_user`**: 先在内核 PT 加载寄存器, 再 `csrw satp` → `sfence.vma` → `sret`

### 页表映射
- `copy_from_kernel`: 创建用户 AS 时复制初始内核映射
- `copy_mappings_from`: 同步最新内核映射到用户 PT（bootstrap 中调用两次: process_init 前+后）

### 已知问题
1. `trap_entry` 未回收旧 sscratch 栈 — 每次 trap 泄漏 64KB (TODO)
2. `restore_and_jump(Trap)` 用 sscratch 暂存 sepc — 嵌套异常时破坏 sscratch (TODO)
3. `restore_context` 对用户任务应分流到 `restore_and_sret` (TODO)
4. `register_task` 对内核任务误设 `user_vdso_base` — 需在设置前判断 pid
5. TrapInfo handler 协程 AXTask 永不释放
6. `VschedStackImpl::dealloc` 时未从 free_stacks 移除, 产生悬空引用 (TODO)


## 测试相关的指令

- make build：编译
- make run：编译并运行
- make test：编译（包括测试文件）并运行（推荐使用这个指令测试）
