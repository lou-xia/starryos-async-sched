# vsched2 移植到 StarryOS — 当前问题与计划

> 平台：RISC-V 64、QEMU virt、当前单核，后续考虑多核。
>
> 当前 vsched2：`main@585b86d`。
>
> 当前版本已重新验证 `Welcome to Starry OS!`；之后在 wait4 replay 与 trap-handler 状态提交冲突处 panic，尚未重新到达 `Hello, World!`。
>
> 完整结构说明见 `core/src/vsched/ARCHITECTURE.md`。

## 1. 当前结论（2026-07-18）

新版 vsched2 已增加原子 `Task::match_set_state`，StarryOS 已用同一份 AxTaskState 承载 `Ready/Running/Blocking/Blocked/Exited` 五态，构建通过。同步 trap 的基础状态流程已经统一为：

```text
trap_entry: Running → Blocked
trap_handler 完成: Blocked → Ready → 入队
```

`585b86d` 的这一主路径与我们的适配一致，但目前仍有两个边界：

1. 当前 handler 只接受旧状态为 `Blocked`，其它状态统一 panic。wait4 Waker 在 `handle()` 返回前把任务改成 `Ready` 时，实测触发 `trap_handler: task state is not Blocked!`；`exit/exit_group` 合法设置 `Exited` 时也会进入同一 panic。正确协议至少要区分 `Blocked → Ready`、已经由其它路径入队的 `Ready` 和不再入队的 `Exited`。
2. 原 wait4 方案依赖 `SyscallOutcome::Pending + Waker` 阻止 handler 自动恢复用户任务；这与新版“同步 trap 完成后统一 Ready”冲突，形成高频 replay、重复入队或状态断言失败。不能再靠 TaskState 隐式表达 syscall completion。

下一步回到统一执行流设计：

```text
SyncRoot + block_on：兼容现有同步 StarryOS 调用链
FutureRoot + await：vsched2/async-os 原生目标
可能阻塞的 syscall：迁移期作为普通 SyscallFlow，不能占用共享 TrapHandlerFlow
```

简化总览见 `core/src/vsched/block_on设计简述.md`，完整分析见 `core/src/vsched/block_on设计文档.md`。

> 说明：第 2～5 节记录了 `585b86d` 之前 wait4 replay 方案的实现与历史验证，当前不再代表最终方案；保留这些内容用于追踪回归原因。

## 2. vsched2 最小修改

vsched2 当前只有两个文件有本地差异：

```text
src/schedule/scheduler.rs
src/schedule/trap_wait_queue.rs
```

统计：

```text
2 files changed, 17 insertions(+), 2 deletions(-)
```

`src/api.rs` 和 `src/interface.rs` 已完全恢复上游，没有本地差异。vsched2 原有注释没有修改。

### 2.1 `Scheduler::pop_task()` 优先级写回

原实现从事件源取出任务后，没有把剩余最高优先级写回 `PROCESS_INFO_TABLE`。这会使 pid=0 的缓存优先级长期保持为旧值，父 wait4 阻塞后仍可能压住子进程。

当前只增加：

```text
没有事件源
  → update_prio(isize::MAX)

取出最高优先级事件源后
  → prio = min(new_prio, second_prio)
  → update_prio(prio)
  → None/Some 都返回同一个 prio
```

这是缓存正确性修复，不是 round-robin 策略。

### 2.2 trap handler 尊重任务状态

原 handler 在 `TrapInfo::handle()` 返回后，会把所有非 Exited 任务强制设为 Ready 并重新入队，因此 StarryOS 无法让 wait4 用户任务保持 Blocked。

当前在原逻辑前增加一个短分支：

```text
task.state() != Running
  → 不自动重新入队
  → 释放当前 TrapInfo
  → 继续处理下一个 TrapInfo
```

含义：

- `Running`：普通 trap 已完成，由 handler 改为 Ready 并入队；
- `Blocked`：StarryOS 正在等待事件，由 Waker 决定何时入队；
- `Ready`：Waker 已经提前完成入队，handler 不重复入队；
- `Exited`：不再入队。

同时补上 `trap_info.dealloc()`。这符合原 `TrapInfo` trait 对生命周期的要求，不需要改变 trait。

## 3. StarryOS 侧实现

### 3.1 wait4 一步检查

StarryOS 内部的 syscall dispatcher 使用：

```text
SyscallOutcome::Complete
SyscallOutcome::Pending
```

这只是 StarryOS 内部返回值，不跨 vDSO 边界，也不修改 vsched2 trait。

wait4 在 vsched2 路径中执行：

1. 检查 interrupt 和 child zombie；
2. 构造持有 `VschedTaskImpl` 稳定指针的 Waker；
3. 注册 `child_exit_event`；
4. 再检查一次 child，覆盖“首次检查与注册之间退出”的窗口；
5. 条件仍未满足时把用户任务设为 Blocked；
6. Waker 进入 armed 状态；
7. 返回 `SyscallOutcome::Pending`，不把临时 `UserContext` 写回 trap frame。

因为 Pending 时不写回 `uctx.ip() = old_sepc + 4`，任务恢复后仍会执行原 wait4 ecall。

### 3.2 Waker 直接唤醒用户任务

Waker 不再寻找 TrapInfo token，而是持有当前被服务用户任务的稳定指针。

Waker 内有三个原子标志：

```text
armed   用户任务是否已提交为 Pending
woken   事件是否已经发生
queued  是否已经执行过重新入队
```

这覆盖两个关键竞态：

```text
wake-before-Blocked/armed
  → 先记录 woken
  → arm 时补做入队

重复 wake
  → queued 保证只调用一次 push_task()
```

真正入队使用 vsched2 已有的 `push_task()` API，不新增 API。

### 3.3 所有用户 trap 统一设为 Running

StarryOS 在进入用户 trap dispatcher 后统一把用户任务状态设为 Running。

这是必要的，因为页错误等非 syscall trap 也需要在处理完成后由 vsched2 handler 自动重新入队；wait4 Pending 分支随后会显式将状态改为 Blocked。

### 3.4 execve 使用现有进程生命周期 API

execve 会替换地址空间和用户 vDSO 私有数据，新的 `USER_SCHEDULER` 尚未初始化。

当前不新增 `process_reinit()`，而是组合已有接口：

```text
old_pid = task.pid
new_pid = process_init(new_vspace)
user_init(new_vspace)
task.pid = new_pid
process_drop(old_pid)
```

trap handler 完成 execve 后，会根据任务的新 pid 将其放入新地址空间的 Scheduler。

运行日志示例：

```text
[execve] vsched pid 2 -> 3
[execve] vsched pid 4 -> 5
[execve] vsched pid 6 -> 7
```

### 3.5 每任务稳定 trap frame

`core/src/vsched/trap_vector.rs` 仍保留每任务稳定 trap frame 修复。

原因：原来的 `TF_POOL[TRAP_COUNT & 3]` 会被其它任务的后续 trap 覆盖。父 wait4 被 Waker 恢复时，必须仍能读取自己的原 ecall frame。

当前：

- 每个 `VschedTaskImpl` 使用一份稳定 `UserTrapFrame`；
- 用户 trap 和 handler yield 原位复用；
- 仅首次没有 frame 时分配；
- 不再每次 yield 泄漏一个 Box。

这属于 StarryOS trap 上下文保存修复，不修改 vsched2。

## 4. 三步完成情况

### 第一步：`pop_task()` 写回

✅ 完成。

父 wait4 进入 Blocked 后，子进程能够立即获得运行。

### 第二步：保留单 handler 的 continuation

✅ 完成，但采用比 Pending TrapInfo 更小的实现。

continuation 由“Blocked 用户任务 + 原 ecall frame”表示，而不是修改 `TrapInfo` trait。每次恢复都会创建一个普通的新 TrapInfo，单 handler 不保存同步阻塞栈。

### 第三步：Waker 到阻塞 continuation 的映射

✅ 完成，但不新增 `woken_pending`。

Waker 直接把 Blocked 用户任务放入 vsched2 已有 ReadyQueue；用户任务本身就是 continuation。

## 5. 验证结果

### 5.1 构建

已通过：

```text
CARGO_TARGET_DIR=/tmp/vsched2-minimal-check \
cargo +nightly-2025-12-12 check \
  --target riscv64gc-unknown-none-elf \
  --locked --offline --features vdso_only

make build
```

只有已有 warning，没有编译错误。

### 5.2 QEMU 日志

父 wait4 与 env：

```text
[clone] push_task pid=2, ok=true
[wait4] PENDING task=0xffffffc08918d080 children=1
[execve] ENTRY pid=10 path=/usr/bin/env
[execve] vsched pid 2 -> 3
[wait4] WAKE task=0xffffffc08918d080
[wait4] ENTRY pid=-1 options=0x0
```

hello_world：

```text
[clone] push_task pid=4, ok=true
[wait4] PENDING task=0xffffffc08918d080 children=1
[execve] ENTRY pid=13 path=./hello_world
[execve] vsched pid 4 -> 5
Hello, World!
[wait4] WAKE task=0xffffffc08918d080
[wait4] ENTRY pid=-1 options=0x0
```

后续 rm：

```text
[clone] push_task pid=6, ok=true
[wait4] PENDING task=0xffffffc08918d080 children=1
[execve] ENTRY pid=16 path=/bin/rm
[execve] vsched pid 6 -> 7
[wait4] WAKE task=0xffffffc08918d080
[wait4] ENTRY pid=-1 options=0x0
```

同时确认：

```text
Welcome to Starry OS!
Hello, World!
```

运行期间未观察到 panic 或 allocator failure。QEMU 由 timeout 主动结束，不是内核失败。

随后又运行了 `make test`：测试用户程序构建成功，测试文件只复制了 release 顶层可执行文件，QEMU 中 env、`Hello, World!` 与 rm 三轮 wait4 均完成。该目标完成后会进入交互式 shell，因此由验证方主动退出 QEMU，不把主动退出视为测试失败。

## 6. 当前边界与后续计划

### 已完成：任务指针 Waker 的 stale 保护

`VschedTaskImpl` 新增 `wake_generation`。Waker 创建时同时保存：

- `WeakAxTaskRef`；
- `VschedTaskImpl` 稳定指针；
- 当前 `wake_generation`。

任务退出时先递增 generation，再设置为 Exited。Waker 入队前要求：

```text
WeakAxTaskRef 仍可 upgrade
且 generation 与创建时一致
且任务状态为 Blocked
```

因此退出后的旧 Waker、重复 Waker 和非 Blocked 任务都不会重复进入 ReadyQueue。

当前 `VschedTaskImpl::dealloc()` 仍为 no-op，所以裸指针物理生命周期尚未结束。将来真正回收 `VschedTaskImpl` 时，仍需把 Waker target 改为拥有引用计数的独立对象，不能只依靠 generation。

多核下 Blocked → Ready 还需要真正的 compare-exchange 状态接口，当前单核实现不宣称已经解决该并发问题。

### 已完成：自动日志断言和日志收敛

新增：

```text
make verify-vsched2
```

该目标运行 `scripts/check-vsched2-log.sh`，短时启动 QEMU，并断言：

```text
Welcome to
[wait4] PENDING task=
[wait4] WAKE task=
path=./hello_world
Hello, World!
```

同时拒绝：

```text
panic in vDSO
memory allocation of
```

已经删除 StarryOS 侧的高频临时输出：`[poll#]`、`[yield]`、`[into_user]`、`[trap]`、`[ecall]`、`[pf]`。vsched2 上游已有的高频日志没有修改，以保持 vsched2 补丁最小。

### P1：其它阻塞 syscall

当前只有 wait4 使用“原 ecall 重试”。后续只有在实际测试命中时才迁移 futex、poll、sleep、pipe 等 syscall。

迁移前需要确认：

- 从原 ecall 重新执行是否幂等；
- 是否已经产生部分副作用；
- 是否需要 StarryOS 侧显式状态机。

不应为了统一形式修改 vsched2 的公共接口。

#### 将整个 `block_on` 改为独立 coroutine 的审计结论（2026-07-17）

结论：**“把现有同步 `block_on<F>() -> F::Output` 透明替换成 coroutine”不应采纳；“把拥有完整状态的 Future worker 作为 vsched2 coroutine 调度”可以作为长期方向。**

理由：

1. vsched2 的 `CoroutinePoll` 是无栈 poll 驱动器，不会自动保存调用者的同步 Rust 栈。Future 放进独立 coroutine 后，原 `block_on()` 调用者仍需要同步返回值；如果调用者是唯一 trap handler，handler 仍然会被占用。
2. 当前调用点并非统一的 `'static + Send` Future。pipe、poll/select、futex、signal、wait queue、mutex 等 Future 会借用 `&self`、用户缓冲区、signal context、文件表或局部闭包状态。把它们搬到全局调度任务需要重新设计所有权、生命周期和取消语义。
3. 通用 worker 的 Waker 必须直接绑定 vsched2 任务，并在 `Blocked → Ready` 时完成 stale generation、重复 wake 和任务注销保护；当前 `AxWaker` 只设置标志位，不能单独承担这个职责。
4. 用户 syscall 还需要一个 completion 对象：Future 完成后写回用户 trap frame、返回值和副作用，并处理 signal、进程退出、部分 I/O 和重复 ecall。这个协议不能由 `block_on()` 函数本身推导出来。
5. 当前 `BLOCK_ON_TOGGLE` / `toggle_handler` 只是单 handler 的实验性栈保存桥：它依赖一个 handler、一个预分配线程栈和一次成对的 toggle，不能表示多个并发 continuation、嵌套阻塞、取消或 handler 重入；因此不能作为通用 coroutine 化方案的依据。wait4 最终路径已经绕过该桥，使用显式 `Pending + Blocked 用户任务`。

可采纳的长期方案是分层的：

- 内核内部阻塞操作：引入拥有 `'static + Send` 状态的 `FutureTask`，由 vsched2 以 `CoroutinePoll` 调度，Waker 直接重新入队该任务，完成后通过 completion 返回结果；
- 用户 syscall：逐个改成显式 `Pending + completion`，Future 完成后写回稳定用户 trap frame，再唤醒用户任务；
- 不能继续保持“所有同步函数调用点不变，只替换 `block_on` 实现”的目标。

因此第 7 步完成为：已审计全部 `block_on` 调用点并否决透明替换方案；当前验证路径只保留 wait4 的显式 continuation。后续只有在具体 syscall 需要时，按上述两类分别迁移。

### P0：`init.sh` 完成后系统不退出（已定位，暂不修复）

现象：`Hello World` 和 `/bin/rm` 均执行完成，但 QEMU 不自动结束，之后看起来像进入了 shell 命令输入阶段。

证据：

- `src/main.rs` 的 `CMDLINE` 是 `/bin/sh -c include_str!("init.sh")`，不是交互式 shell；
- `src/init.sh` 第 14–18 行依次执行 `hello_world`、`rm -rf /tests` 和 `exit`，第 19–20 行的 `cd ~` / `sh --login` 是注释；
- 日志已经出现最后一轮 `[wait4] WAKE`、`[wait4] ENTRY` 和 `trap_handler: task is Exited, skipping push`，没有新的 `read`、交互 shell `execve` 或 panic；
- `sys_exit` / `sys_exit_group` 通过 `do_exit()` 后调用 `mark_exited()`，对应的 init 用户任务被标记为 `Exited`，trap handler 按设计不再重新入队；
- 但 `vsched2_bootstrap()` 的返回类型是 `!`，末尾是无条件的 `loop { call vsched_yield_trampoline }`；`src/main.rs` 中位于其后的 SBI shutdown 代码因此永远不可达。

当前最可能的根因是：init shell 已经正常退出，用户任务从调度器消失，但内核 bootstrap 主任务继续无条件 yield，系统没有“init 已退出 → 关闭 QEMU/SBI shutdown”的控制路径。也就是说，观察到的命令输入终端更可能是 QEMU 仍保持打开，而不是 `init.sh` 真正进入了交互 shell。

后续修复需要先决定生命周期策略：可以让 bootstrap 在确认 init 任务 `Exited` 后返回到 `main` 的 shutdown 路径，也可以在 init completion 回调中直接触发 SBI shutdown；无论采用哪种方式，都必须同时处理 init 的裸指针生命周期、handler/栈回收和没有其它用户任务时的调度器空转。本轮只记录原因，不修改运行逻辑。

### P1：同优先级进程公平

当前策略仍优先保留同优先级 current process，不是公平轮转。

它不是 wait4 正确性的前提。后续如需公平，建议使用每 CPU cursor，只在当前最高优先级集合中轮转，并独立测试。

本轮暂不实施：仅按 pid 顺序扫描虽然改动很小，但内核调度入口通常以 pid 0 作为 current process，不能形成可靠的跨入口轮转；还需要定义进程注销、优先级变化和多核同步时 cursor 的语义。直接改动该策略会超出 wait4 修复所需的最小 vsched2 补丁。

### P1：用户态中断

vsched2 用户态中断分支仍有：

```text
todo!("用户态中断处理流程")
```

命中后会 panic，应在多核之前处理。

### P1/P2：资源生命周期

需要继续审计：

- execve 创建的新 handler/栈与旧 vDSO 资源；
- `process_drop()` 后旧地址空间资源；
- `VschedTaskImpl` 与稳定 trap frame 回收；
- process slot 和用户 vDSO 页释放；
- 多核 per-CPU 状态与锁顺序。

## 7. 下一步顺序

1. ✅ `Scheduler::pop_task()` 优先级写回；
2. ✅ 单 handler 下用 Blocked 用户任务保存 wait4 continuation；
3. ✅ Waker 使用已有 `push_task()` 事件驱动唤醒用户任务；
4. ✅ QEMU 验证 env、`Hello, World!`、rm 三轮 wait4；
5. ✅ Waker 增加 `WeakAxTaskRef + wake_generation + TaskState` stale 保护；
6. ✅ 增加 `make verify-vsched2` 自动日志断言，并清理 StarryOS 高频临时日志；
7. ✅ 审计通用 `block_on` coroutine 方案和全部调用点；当前验证路径无需迁移其它 syscall；
8. ⏳ 修复 init 任务退出后的 bootstrap/SBI shutdown 生命周期；
9. ⏳ 独立实现并测试同优先级公平策略；
10. ⏳ 处理用户态中断与资源回收；
11. ⏳ 开始多核验证。

## 8. 已解决历史问题摘要

| 问题 | 状态 |
|------|------|
| `ktask_schedule(pid != 0)` 调试 `unreachable!()` | ✅ 上游已删除 |
| `switch_vspace()` 调试 `unreachable!()` | ✅ 上游已删除 |
| `Scheduler::pop_task()` 旧优先级缓存 | ✅ 最小修复 |
| wait4 占用唯一 handler | ✅ Blocked 用户任务 + 原 ecall 重试 |
| Waker 无法唤醒 wait4 | ✅ 已有 `push_task()` 直接入队 |
| Pending frame 被 `TF_POOL[4]` 覆盖 | ✅ 每任务稳定 frame |
| yield 每次泄漏 trap-frame Box | ✅ 原位复用 |
| execve 新 vDSO Scheduler 未初始化 | ✅ 已有 process API 组合 |
| `Welcome to Starry OS!` | ✅ 当前验证 |
| `Hello, World!` | ✅ 当前验证 |
| vDSO 日志 `config.log=true` | ✅ template 自动初始化日志桥 |
| vDSO panic 无输出 | ✅ template panic 日志 |
| `make test` 复制整个 target 导致 ENOSPC | ✅ 只复制 release 顶层可执行文件 |
