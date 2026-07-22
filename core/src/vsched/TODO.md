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

### 已完成：StarryOS 接口契约修复（2026-07-21）

本轮逐项对照了 vsched2 的 `Task / Stack / Context / TrapInfo / SMP / VSpace / UserData` 约定，修复均位于 StarryOS，没有修改 vsched2，也没有改动 block_on、TrapHandler continuation 或用户态 yield 方案。

已完成内容：

1. `Task::is_kernel()` 与 `Task::pid()` 已解耦。`is_kernel` 只表示运行特权级，`pid` 只表示所属地址空间，因此现在可以表达短期方案需要的 `SyscallTask { is_kernel=true, pid=user_pid }`。
2. 普通内核线程注册时统一分配 vsched2 `Stack`，并构造首次进入 axtask entry closure 的初始 frame；每次恢复普通内核线程时同步安装对应的 per-CPU `axtask::current()`，避免两个内核线程交替运行时看到陈旧 current。该通用入口已通过编译，但仍需在真正引入 SyscallTask 后做运行测试。
3. `TrapInfo::handle(Some/None)` 已按约定实现：同步 trap 使用 `Some(task)`，外部中断使用 `None`；`TrapInfo` 自身持有完整、不可变的 `UserTrapFrame` 快照，dispatcher 不再用任务中可能被后续 trap 覆盖的 frame 判断本次事件。实际 syscall 返回值仍写入任务的稳定 resume frame。
4. `LAST_TRAPPED_USER_TASK` 和 `TRAPPED_VSCHED_TASK` 只在对应 dispatcher 活跃期间保存，并使用 compare-exchange 清除，降低陈旧裸指针被后续 trap 误用的风险。
5. `CURRENT_VDSO_BASE`、`TRAPPED_VSCHED_TASK`、`LAST_TRAPPED_USER_TASK`、`HANDLER_STACK` 的长度统一改用 `axconfig::plat::CPU_NUM`；`SMP::cpu_id()` 增加越界断言。真正启用多核前仍需核对 StarryOS 与 vsched2 的编译期 `CPU_NUM` 完全一致，并做并发时序测试。
6. `Stack::dealloc()` 增加 magic 校验和释放前失效处理；`Context::into_user_context()` 对空任务、空 frame、`SPP != 0` 和非法用户 `sepc` 改为明确断言，不再静默自旋或意外返回 S 态。
7. init/clone 传给 `process_init()` 的 `AddrSpace*` 已直接使用稳定地址，删除了没有所有权意义且不会释放的 `Box<*mut AddrSpace>` 包装。
8. `VSpace::dealloc()` 保持 no-op，但所有权约定已经明确：传入 vsched2 的指针借用自 `ProcessData`，vsched2 没有取得额外 `Arc` 强引用；`process_drop()` 释放 scheduler slot，地址空间仍由 `ProcessData` 回收。

#### `UserData` 的上游约定不一致

接口注释规定 `Some(vspace)` 是 OS 定义的地址空间指针，但 vsched2 当前的 `current::get_user_data()` 会把 `None` 改写为：

```text
Some(CURRENT_VSPACE as *mut ())
```

这里实际传入的是 pid 小整数。若 StarryOS 严格把它解释为 `AddrSpace*`，启动时会在 `get_user_data()` 访问地址 `0x79` 并触发 load page fault。

当前兼容策略是：

- `Some(kernel pointer)`：按显式 `AddrSpace*` 查询每一页，验证完整范围可访问且物理映射连续，再返回 KVA；
- `Some(small pid)` 或 `None`：按当前已激活地址空间处理，通过 per-CPU `CURRENT_VDSO_BASE` 得到 UVA；
- vVAR 仍直接返回内核共享映射。

长期应由 vsched2 统一接口注释和 `current::get_user_data()` 的实际参数语义；在上游统一前不能删除 small-pid 兼容。

#### 本轮明确保留的接口边界

- `Task::dealloc()` 仍为 no-op。wait4 Waker 和 vsched2 StackHandler 仍可能保存任务/栈裸指针，直接回收会制造 use-after-free；需要与 block/waker 生命周期一起设计 deferred reclaim，本轮不处理。
- `HANDLER_STACK` 虽已改成 per-CPU 数组，但每 CPU 仍只有一个槽，多个 handler 会互相覆盖。这属于现有 `BLOCK_ON_TOGGLE`/handler continuation 设计，本轮不处理。
- `VSpace::dealloc()` 的 no-op 是当前借用式所有权协议，不是遗漏；若以后希望 vsched2 独立持有地址空间，需要把传入值改成拥有引用计数的句柄并成对释放。

#### 最新运行验证

最新镜像已重新到达：

```text
Welcome to Starry OS!
[clone] push_task pid=2, ok=true
[wait4] PENDING ...
```

严格解释 `UserData` 时出现的 `stval=0x79`、`sepc=get_user_data` 页错误已消失，也没有再次出现由该错误引起的 `no thread, no last user`。随后系统停在已知问题：

```text
panic in vDSO: trap_handler: task state is not Blocked!
```

该停止点属于暂缓处理的 block_on/TrapHandler 状态问题，因此本轮最新镜像尚未重新到达 `Hello, World!`；不能把这次 timeout 记为完整功能验证通过。

`make test` 的用户测试程序构建、复制到 `disk.img` 和内核 release 构建均成功；QEMU 运行同样在上述已知断言处停止，验证方随后主动中断 QEMU。因此本轮 `make test` 只能记为“构建阶段通过、运行阶段被已知 block_on/handler 问题阻断”。

### P0：用户线程尚不能通过 `utask_schedule()` 在 U 模式直接切换

预期场景是：

```text
同一地址空间中的用户线程 A
  → yield → 用户态 uschedule/utask_schedule → 线程 B
  → yield → 用户态 uschedule/utask_schedule → 线程 A
```

当前 StarryOS **不能满足**。现有实际流程是：

```text
A 执行 Linux sched_yield ecall
  → 进入内核 TrapHandler
  → sys_sched_yield() 调用 axtask::yield_now()
  → 内核 vsched_yield_trampoline
  → raw_thread_entry → kschedule
  → 内核选择 B 所在的用户 Scheduler
  → krun_utask → into_user_context → sret 到 B

B 再次 sched_yield
  → 重复进入内核和 kschedule
```

这里 yield 时，`axtask::with_current_task()` 只临时替换 axtask 的 current；vsched2 的 `CURRENT_TASK` 仍是共享 TrapHandler，`IN_KERNEL` 也为 true。因此被保存和让权的是内核 handler，不是用户线程 A，分支只能进入 `kschedule`，不会进入 `uschedule/utask_schedule`。而且 handler 仍标记为 coroutine 时在 syscall 内普通 yield，会丢失当前 `TrapInfo::handle()` continuation，不能作为用户 yield 的桥。

即使为用户程序增加对 `raw_thread_entry` 的直接调用，当前仍有以下闭环缺口：

1. Linux `sched_yield` 本身是 ecall；若要完全不陷入内核，需要 libc/用户运行时改为调用用户 vDSO yield 入口，同时保留 syscall fallback。
2. StarryOS 的 `Task::resched()` 只会进入内核地址的 `vsched_yield_trampoline`，并保存 S 模式 frame（`SPP=1`），没有 U 模式线程上下文保存入口。
3. 用户 vDSO 的 `Task_TABLE` 等 trait vtable 位于每进程私有 `.bss`，当前只初始化了内核 vDSO 副本；其中需要的 StarryOS 回调又是内核代码地址，U 模式不能调用。
4. `Scheduler::init_sources()` 当前从内核 vDSO 执行，写入用户 Scheduler 的 `EventSourceVtable` 函数指针也是内核 vDSO 地址；字段偏移解决了 KVA/UVA 数据自引用，但没有解决函数指针的地址基址。
5. 用户任务对象是内核堆中的 `VschedTaskImpl/AxTaskRef`，`trap_frame` 和 `thread_stack_ptr` 也指向内核对象；U 模式 vDSO 无法直接调用其状态、优先级和上下文接口。
6. 当前 `Stack::alloc()` 只分配内核堆栈，不能作为 `Context::into_user()` 传入的 U 模式协程栈。
7. vsched2 的 `thread_entry()` 会调用 RISC-V `assert_disable_irq()` 读取 `sstatus`，而 U 模式不能访问 S 态 CSR；同时 `raw_thread_entry` 注释要求关中断进入，普通 U 模式代码也无法直接满足该前提。

因此，当前的“用户 Scheduler 已初始化、内核可从中取出用户任务”不等于“用户 Scheduler 已能在 U 模式自行运行”。要实现预期场景，需要先确定并与 vsched2 对齐一套真正的用户态 ABI：

1. 增加 U 模式可调用的 yield/context-save 入口，保存 A 的用户线程上下文并在进入 `raw_thread_entry` 前把 A 置为 Ready；
2. 为用户侧提供 U 可访问的最小 Task/Context/Stack 表示和 U 地址函数入口，不能直接暴露 `AxTaskRef` 或内核函数指针；
3. 将用户 Scheduler 的事件源 vtable 按用户 vDSO 基址初始化，或改成与字段相同的相对偏移；
4. 拆分 `thread_entry` 的内核/用户前置条件，用户路径不能读取 S 态 CSR，也不能依赖 U 模式自行关 S 中断；
5. 完成后新增同地址空间两个线程反复 yield 的专门测试，并用日志断言：第一次进入用户调度循环后，同 pid 切换不出现 ecall `0xdead`，只有选中内核 Scheduler 或其它 pid 时才通过 `Context::into_kernel()` 陷入内核。

在上述闭环完成前，迁移期应明确接受 `sched_yield → trap → kschedule` 的内核调度路径，不能把当前状态描述为已经使用了 `utask_schedule`。

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

`Task::match_set_state()` 已由 axtask 的 CAS 循环实现原子状态转换；但 Waker 的队列插入/失败回滚、任务裸指针生命周期和跨 CPU 入队仍未完成多核验证，当前不能据此宣称阻塞唤醒已经支持多核。

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
8. ✅ 修复 StarryOS 的 vsched2 接口实现与约定问题；保留与 block/waker 生命周期绑定的回收项；
9. ⏳ 继续讨论并修复 block_on/TrapHandler 状态问题；
10. ⏳ 修复 init 任务退出后的 bootstrap/SBI shutdown 生命周期；
11. ⏳ 独立实现并测试同优先级公平策略；
12. ⏳ 处理用户态中断与资源回收；
13. ⏳ 开始多核验证。

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
| `Hello, World!` | ✅ 历史版本已验证；最新版本被已知 handler 状态问题提前阻断 |
| vDSO 日志 `config.log=true` | ✅ template 自动初始化日志桥 |
| vDSO panic 无输出 | ✅ template panic 日志 |
| `make test` 复制整个 target 导致 ENOSPC | ✅ 只复制 release 顶层可执行文件 |
| `Task::is_kernel()` 错误依赖 `pid == 0` | ✅ 独立特权级字段 |
| 普通内核线程无法首次进入/恢复 axtask entry | ✅ 初始 frame + external-current 桥 |
| `TrapInfo::handle()` 忽略 `Some/None` 且快照不稳定 | ✅ Option 语义 + 完整 frame 快照 |
| `UserData` 把 vsched2 传入的小整数 pid 当指针 | ✅ 临时兼容 pointer/pid 两种实际调用 |
| StarryOS vsched per-CPU 数组硬编码为 1 | ✅ 改用 `axconfig::plat::CPU_NUM` |
