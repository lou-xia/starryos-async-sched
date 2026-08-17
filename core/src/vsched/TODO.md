# vsched2 移植到 StarryOS：状态、路线与 TODO

> 平台：RISC-V 64、QEMU virt。先稳定单核，再启用多核。
>
> 当前基线：StarryOS `26f7e08`；vsched2 `5738b48`（最新 `main`，含本地四文件适配）。
>
> 架构和文件索引见 `ARCHITECTURE.md`。本文只记录当前有效设计、待办事项和验收标准；
> 已撤销的 SyscallTask/`TrapHandlerResult` 试验不再作为实现基础。

## 1. 当前基线（2026-07-27）

### 1.1 已完成

- `Scheduler::pop_task()` 的最高优先级写回问题已由当前 vsched2 修复；
- vsched2 `6814290` 已将 `TrapInfo::new_handler()` 的 opaque 参数从 `TrapWaitQueue` 改为
  `Scheduler`，并在 handler 取空当前 trap 队列时更新调度器优先级；StarryOS 只透传该指针，
  无须改变 VTABLE；
- vsched2 `5738b48` 已将进程最高优先级改为 per-CPU，并由
  `EventSourceVtable::is_prio_per_cpu` 区分共享优先级和 per-CPU 优先级；本地等待复查已相应读取
  `highest_prio[cpu_id]`；
- StarryOS 已能使用预编译 vDSO VTABLE 调用 vsched2；
- `vdso_crate_template` 已支持由 `build.rs` 的 `config.log=true` 自动初始化日志桥，并能输出
  panic；vsched2 无须为日志做本地修改；
- `Task::is_kernel()` 已与 `Task::pid()` 解耦；StarryOS 单页表模型不再使用
  `pid != 0 => 用户任务` 的错误判断；
- `wait4` 的 `block_on()` 已能保存并恢复当前 TrapHandler continuation；
- TrapInfo 使用每任务稳定 trap frame，不再被共享 frame 覆盖；
- 已实现 per-CPU TrapInfo 队列和所有 CPU 共享的可复用 TrapHandler 池；
- 单个 handler 因内核资源阻塞时，其他 handler 可以继续处理后续 TrapInfo；
- `Welcome to Starry OS!`、旧启动脚本中的 `Hello, World!` 和 wait4 接力均已验证；
- `make verify-vsched2` 当前通过单核自动日志验证；
- UART、timer、software/IPI 的 pending 抑制已放在 StarryOS 低层 trap 入口；
- tty-reader 已由 vsched2 创建和调度，不再遗留在 AxRunQueue；
- vsched2 无任务时已能在调度循环中临时开中断并执行 WFI，不再需要额外 idle 线程；
- 内核根协程已有 `IrqCorotineWrapper` 保存/恢复自己的本地 IRQ 状态；
- 根协程被 IRQ 打断后会临时转为线程保存 continuation，恢复后再转回协程；
- 已登记初始用户任务的 vsched2 身份；该任务提交 `Exited` 后直接执行系统关机，恢复了
  旧启动脚本中 `exit` 结束 QEMU 的生命周期语义，普通子进程退出仍只唤醒 `wait4`；
- 线程主动让权时已统一在 StarryOS yield 入口移交 continuation 栈；vsched2 对线程在单核和
  多核都先切到独立 empty stack，再提交状态和进入调度循环。
- vsched2 syscall dispatcher 已在构造 `UserContext` 时复制完整 trapped GPR；传统 `fork()`
  child 不再因 `gp=0` 在首个 syscall 前访问 `0x194`，最小 `_exit/waitpid` 回归通过；
- `AddrSpace::try_clone/clear`、fork 私有 vDSO 重映射和 execve 已形成完整 `vdso_base`
  生命周期；`AddrSpace.vdso_base` 与 `VschedTaskImpl.user_vdso_base` 在任务可运行前保持同步。
- P0-3 阶段 C 已完成：独立的 `no_std` `vsched_abi`、共享任务槽、generation 句柄、
  内核 registry、编码任务 ID 和用户 Task VTABLE 已形成闭环；用户态上下文切换仍留在阶段 D；
- `process_init()` 返回的真实 vsched2 process id 已持久绑定到 `ProcessData`，init、fork、execve
  和最后线程退出形成注册/替换/注销闭环；`CLONE_THREAD` 继承同一 id，并进入同一
  `USER_SCHEDULER`。`std::thread::spawn -> join -> clear_child_tid` 回归已通过。

### 1.2 当前验证边界与直接阻塞点

当前 `src/init.sh` 已改为交互式 `sh --login`。此前在 shell 第一次阻塞读取后出现的：

```text
starry:~# [block_on] coroutine -> thread task=...
panic in vDSO: Failed to take current stack
```

已经修复。准确原因不是 UART/Waker，而是 block_on 已把 handler continuation 栈交给任务，
单核 `thread_entry()` 却没有切换到独立调度栈；scheduler-root 仍在该任务栈上进入 WFI，
栈管理元数据已经是 `current_stack=None`。当前由统一 yield 入口完成线程栈移交，vsched2 的
`thread_entry()` 对线程不再按 CPU 数量区分，先切到 empty stack 后才运行 phase2。第 4.6 节
记录完整流程和修复后的所有权边界。

本轮曾临时恢复仓库默认的 `hello_world -> rm -> exit` 脚本执行 `make test`：所有 wait4
接力完成，init 的 `exit` 后 QEMU 正常关闭；测试后已恢复交互脚本。因此系统生命周期问题
已修复。随后交互测试完成一次 `echo STACK_OK`、shell `exit` 和 init 退出，QEMU 正常关闭，
期间 block_on 成功完成“协程 -> 线程 continuation -> 协程”的往返，未再出现 vDSO panic。
测试 PTY 本身不会自动回复 BusyBox 发出的 `ESC[6n`；自动交互时需先发送标准 CPR，这不是
StarryOS 的输入故障。

2026-07-27 更新到 vsched2 `5738b48` 后，没有 cherry-pick 旧 `dev`，而是在最新 `main` 上
重新落入 `api.rs`、`arch/riscv.rs`、`current.rs`、`main_loop.rs` 四个必要适配文件；上游新增的
`scheduler/event_source/process_info/ready_queue/trap_wait_queue` 逻辑均保持原样。验证结果：

- `make build`、`make verify-vsched2` 和 `make test` 均能完成其构建阶段；
- 交互 shell 可输入，`echo STACK_OK`、`pwd`、相对路径 `mkdir`、重定向和 `cat` 正常；
- 直接运行测试镜像中的 `hello_world` 输出 `Hello, World!`；
- shell `exit` 后父 init 的 wait4 完成，QEMU 正常退出；
- 全程未复现 `Failed to take current stack`、非法 handler 状态或 vDSO panic。

扩展测试中的管道退出链已于 2026-08-01 修复：`echo PIPE_OK | cat` 输出数据后，写端随退出
任务回收而析构，`cat` 读到 EOF，父 shell 完成 wait4 并返回提示符。根因不是 Pipe 或
`block_on`，而是 TrapHandler 处理 `exit` 后只跳过了 Exited 用户任务的重新入队，没有调用
`Task::dealloc()`；同时 StarryOS 的该接口仍为空实现。当前剩余的独立文件系统边界是
`mkdir -p /tmp/...` 会因尝试创建 `/` 得到 `EINVAL`，相对路径 `mkdir` 正常。

当前多核直接阻塞在构建配置传播：`make SMP=4 build` 生成的 StarryOS 为 4 核，但 vsched2
vDSO 内的 per-CPU 数组长度仍为 1。四核启动时 hart 1/2/3 因此在 `api.rs` 访问数组越界，
另有副核在 `VschedTaskImpl::execution_task()` 触发页错误。修复 CPU_NUM 配置闭环后，才能
区分后者是独立的副核初始化问题还是数组越界的连带破坏，并验证本轮 per-CPU 等待上下文、
IRQ wrapper 和远程唤醒的并发行为。

### 1.3 当前改动边界

当前 TrapHandler 方案保持以下边界：

- 不修改 `TrapInfo::handle()` 的签名和 vDSO/VTABLE ABI；
- 不恢复 `Completed/Waiting/Deferred` 等返回枚举；
- 不为每次 syscall 创建一次性 SyscallTask；
- TrapInfo 队列保持 per-CPU；
- handler 不属于某个 CPU，空闲 handler 位于全局共享池；
- 尽量只在 StarryOS 侧补齐中断、任务创建、Waker 和执行上下文适配；
- block_on/yield 的通用改造按实际调用类别逐步验证，不在修复终端输入时一次性重写。

## 2. 当前 TrapHandler 与 block_on 模型

### 2.1 TrapHandler 共享池

```text
CPU0 收到 TrapInfo
  -> queues[0] 入队
  -> 从全局 idle_handlers 取 H1；没有则按需创建
  -> H1 首次在 CPU0 运行，连续处理 CPU0 当前队列

H1 在处理 syscall 时 block_on Pending
  -> H1 保存 continuation
  -> H1 进入相应内核资源等待队列，不进入 idle_handlers
  -> queues[0] 仍有工作时，取出或创建 H2 继续处理

资源 Waker 唤醒 H1
  -> H1 可在任意 CPU 从原调用位置恢复
  -> 完成自己已经领取的 TrapInfo
  -> 继续处理恢复所在 CPU 的队列
  -> 队列为空后进入全局 idle_handlers
```

不存在 H1/H2 对后续工作的固定所有权。谁从加锁的 `queues[current_cpu]` 成功取到
TrapInfo，谁处理它。

空闲池中的状态协议为：

- `Blocked -> Ready`：`take_task()` 可以直接返回该 handler；
- `Blocking -> Ready`：上下文仍在保存，本次不直接运行，由保存方在安全点完成唯一一次
  ReadyQueue 入队；
- `Ready/Running`：不允许出现在 `idle_handlers`，出现即为重复入队或状态损坏；
- `Exited`：从共享池丢弃，后续再做安全回收。

`park_handler()` 中将 `pid` 设为 0 的准确含义是：handler 完成当前 TrapInfo 后，在空闲期
重新归属于 0 号内核进程。它不是“未绑定地址空间”。是否切换页表由
`PROCESS_INFO_TABLE[0].vspace` 以及 OS 的单页表/双页表实现决定。handler 处理资源阻塞时
不会经过 `park_handler()`，所以 continuation 仍保留当前服务进程的 pid。

### 2.2 用户任务、handler 和地址空间

同步异常/syscall 的状态流程为：

1. vsched2 在 `trap_entry` 中把被 trap 用户任务从 `Running` 改为 `Blocked`；
2. handler 领取 TrapInfo，并切换到该任务 pid 对应的地址空间；
3. StarryOS dispatcher 同步调用现有 `handle_syscall()`；
4. 快速完成时，`handle()` 返回，vsched2 才将用户任务 `Blocked -> Ready`；
5. `block_on Pending` 时，阻塞的是 handler，用户任务继续保持 `Blocked`；
6. Waker 恢复同一个 handler，handler 写回 trap frame；
7. 只有 `handle()` 真正返回后，用户任务才能恢复；若 syscall 已将任务设为 `Exited`，则
   不再入队。

现有 syscall 代码依赖 `axtask::current()` 获取进程、FD、信号和地址空间，因此 handler
处理用户 TrapInfo 时使用动态执行身份：

```text
vsched2 current       = TrapHandler H
axtask current        = 被服务的用户任务 U
H.trap_owner          = U
```

handler 让权前释放该执行上下文，continuation 恢复后重新安装；完成 TrapInfo 后清除绑定并
递增 Waker generation。嵌套内核 trap 通过有界 `trap_owner` 链找到真正用户 owner。

### 2.3 block_on/Waker 握手

```text
Future::poll -> Pending
  -> AxWaker: Idle -> Parking
  -> handler: Running -> Blocking
  -> AxWaker: Parking -> Parked
  -> 协程 handler 保存当前真实栈，临时成为带 continuation 的线程
  -> vsched2 在上下文安全后提交 Blocking -> Blocked

资源完成
  -> AxWaker: Parked -> Notified
  -> handler: Blocking/Blocked -> Ready
  -> 恢复原栈和原调用位置
  -> handler 恢复协程身份
```

四态 AxWaker 用于封闭 `poll` 返回 Pending 与真正完成阻塞之间的丢失唤醒窗口。普通
vsched2 内核线程已有持久栈，只使用相同的状态/Waker 握手，不需要协程到线程转换。

## 3. 总体实施路线

按以下顺序推进。每个阶段先满足自己的验收标准，再进入下一阶段。

### 阶段 1：修复 BusyBox 交互输入（最小交互闭环已完成）

恢复完整链路：

```text
UART 字符 -> supervisor external interrupt -> TrapInfo(None)
  -> TrapHandler -> axhal IRQ/PLIC claim
  -> UART IRQ hook -> 唤醒 tty-reader
  -> tty-reader 读取 UART 并唤醒终端 PollSet
  -> 唤醒 read syscall 所在 handler continuation
  -> read 返回 -> 用户 shell Ready -> BusyBox 收到字符
```

详细方案见第 4 节。

### 阶段 2：验证 BusyBox 与基础启动流程

验证：

- `cd`、`pwd`、`ls`；
- `mkdir`、`rmdir`、`touch`、`rm`；
- `cat`、`echo`、输入/输出重定向；
- 单级和多级管道；
- fork/exec/wait4/exit；
- 空闲等待后再次输入、重复执行命令、Ctrl-C 和基本信号；
- 多次命令后的 handler、Waker、FD、进程和栈生命周期。

### 阶段 3：打通用户态 vDSO 线程切换并验证 `utask_schedule()`

当前只能确认 vDSO 已映射到用户空间、vsched2 已有 `raw_uschedule/uschedule/utask_schedule`
控制流，不能宣称已经实现“不进入内核的用户线程切换”。先用读取时间等无状态函数验证用户
程序可以解析并调用 vDSO，且没有发生 ecall；再按第 5.3 节补齐同地址空间用户线程注册、
用户可访问的调度控制块、U 模式上下文切换入口和运行时接入。

核心用例：

```text
线程 A 在用户态 yield
  -> utask_schedule 选择同地址空间线程 B
  -> B 在用户态 yield
  -> utask_schedule 恢复 A
```

验收要求：页表不变，用户态本地切换计数增加，trap/ecall 计数不增加；无本地 Ready
任务、需要跨地址空间、存在待处理信号或退出时能可靠回退内核。

### 阶段 4：启用多核

先 `SMP=2`，再执行 `SMP=4 make test`。依次完成：

- 副核 `kernel_init_secondary()`；
- 每 hart 的 `stvec`、`sscratch`、current task 和 vsched2 per-CPU 数据；
- 正确的 per-CPU `gp`，不能继续使用全局单值；
- per-CPU readiness，替代全局 `VSCHED2_READY/VSCHED2_YIELD` 假设；
- timer、external interrupt、IPI、remote wake 和 idle/WFI；
- affinity、任务迁移和 timer owner；
- 共享 handler 池的多核竞争及 AxWaker 的跨核唯一入队。

### 阶段 5：逐类适配 syscall/Future

短期继续让可复用 TrapHandler 同步调用 `handle_syscall()`，在叶子 Future 返回 Pending 时
保存 handler continuation。按类别验证并逐步改为真正 async/await：

1. sleep/timer、wait4、futex；
2. pipe、eventfd、signalfd、WaitQueue；
3. 普通文件与磁盘 I/O；
4. poll/select/epoll；
5. signal、取消、超时、exit/execve；
6. socket、网络和条件设备后台任务。

最终目标是让 vsched2 可见的 TrapHandler/协程执行流直接等待叶子 Future；同步 syscall
仍在一次激活中快速完成。当前阶段不恢复“每 syscall 创建一个 SyscallTask”的方案。

## 4. 已实施：无任务 WFI 与根协程 IRQ 适配

### 4.1 原问题与旧 init.sh 能运行的原因

此前交互脚本的日志停在：

```text
starry:~# [block_on] coroutine -> thread task=...
```

这表示 BusyBox 的阻塞式 `read(0, ...)` 已经进入 handler 的 block_on，问题发生在资源事件
没有把 continuation 唤醒，而不是 syscall 返回值或 BusyBox 命令解析错误。

旧 `init.sh` 的命令通过：

```text
/bin/sh -c include_str!("init.sh")
```

作为 argv 中的字符串传给 shell。shell 直接解析内存中的 `-c` 参数，不需要从 fd 0 读取，
所以即使 UART IRQ、tty-reader 和 idle 唤醒链都不可用，脚本仍然可以执行。

### 4.2 已修复的三个断点

#### 断点 A：中断 dispatcher 丢弃了全部中断（已修复）

`api/src/task.rs::vsched_trap_dispatcher()` 现在把完整 `scause` 交给
`axhal::irq::irq_handler()`。不能只传低位 cause，否则平台层无法区分 supervisor
timer/software/external interrupt。external interrupt 仍形成 `TrapInfo(None)`，随后完成
PLIC claim、设备 handler/IRQ hook 和 complete。

低层入口在延迟处理前先抑制同一 pending 源：

- timer：把 `stimecmp` 暂设为 `u64::MAX`，实际 handler 再安装下次 deadline；
- external：只清本 hart 的 `sie.SEIE`，PLIC complete 后恢复；
- software/IPI：清本 hart 的 `sip.SSIP`。

否则从硬件 trap 到 TrapHandler 真正运行之间一旦开中断，未清的 pending 位会立即重入。

此前缺少分发时会导致：

- supervisor external interrupt 虽然形成 `TrapInfo(None)`，却没有调用
  `axhal::irq::irq_handler()`；

- PLIC 不会 claim UART IRQ；
- UART handler/IRQ hook 不运行；
- PLIC 不会 complete；
- `register_irq_waker()` 中的 PollSet 不会 wake。

RISC-V `axplat-riscv64-qemu-virt` 的 IRQ 接口要求传入完整 `scause`，包括最高位。正式实现
不能只传 `scause & mask`；否则 `S_TIMER/S_SOFT/S_EXT` 分类会失效。

#### 断点 B：tty-reader 仍在旧 AxRunQueue（已修复）

`api/src/terminal/ldisc.rs` 已改用 `starry_core::vsched::spawn_kernel_thread()`。在 vsched2
初始化前创建的后台任务进入显式 pending 列表，`kernel_init_main()` 完成后才统一注册到
vsched2 kernel ReadyQueue；初始化后的新任务直接注册。任务不会同时属于 AxRunQueue 和
vsched2。

#### 断点 C：没有可被 IRQ 唤醒的 vsched2 idle 边界（已修复）

当用户任务、handler 和 tty-reader 全部阻塞后，`kschedule()`/`utok_schedule()` 现在进入
`wait_for_runnable_task()`：

1. 保持本地中断关闭；
2. 把 `CURRENT_TASK[cpu]` 发布为该 CPU 初始化时传入的稳定等待上下文；
3. `SeqCst fence` 后复查全局最高优先级，避免在发布等待状态前已经入队的任务被漏掉；
4. 仍无任务才执行 `enable IRQ -> wfi -> disable IRQ`；
5. IRQ 从 trap 路径非局部进入调度器，因此不依赖不会执行 Drop 的 RAII guard。

vsched2 只新增一个必要的 per-CPU 全局数组 `SCHEDULER_WAIT_CONTEXT`。没有另设 waiting bool；
当前是否处于等待根通过 `CURRENT_TASK[cpu] == SCHEDULER_WAIT_CONTEXT[cpu]` 判断。

多核发布者仍必须在远程任务入队后发送 IPI。发布后复查只封闭“本 CPU 即将睡眠”窗口，
不能替代 remote wake/IPI。

### 4.3 阶段 A：等待根的栈轮换

普通任务 IRQ 完全保留 vsched2 当前设计：

```text
被打断任务栈 = _old
原 sscratch/trap_stack = current_stack（本次中断处理使用）
新分配栈 = 下一次 sscratch/trap_stack
```

任务恢复时，若选择另一个协程则可复用 `current_stack`；选择其它线程则安装其线程栈并
回收 `current_stack`；恢复被打断线程则使用 `_old`，并回收中断处理栈。本轮没有改变这条
普通路径。

等待根没有任务 continuation，若仍按普通路径每次分配新 trap 栈，timer IRQ 会持续泄漏
栈并最终 OOM。因此只在等待根做双栈轮换：

```text
中断前：current_stack = WFI/调度栈 S，trap_stack = T
中断后：current_stack = T（处理中断），trap_stack = S（下次 trap）
```

这条特例不会把等待上下文设为 Ready，也不会把它推入 ReadyQueue；它只为
`TrapInfo::from_task()` 提供稳定快照。

### 4.4 阶段 B：`IrqCorotineWrapper`

所有 StarryOS 内核根协程在注册时自动包一层 `IrqCorotineWrapper`；用户协程暂不包装。
wrapper 不增加全局变量，每个根协程保存自己的 SIE 状态：

```text
进入 poll（调度器保证 IRQ disabled）
  -> 恢复该协程上次保存的 SIE
  -> poll 内层 Future
  -> 保存当前 SIE 并关闭 IRQ
  -> 把 Poll 返回给 vsched2
```

不能把 `IrqSave` guard 对象跨 poll 保存，因为 IRQ 或主动 yield 会从当前调用链非局部离开，
不能保证 guard 的 Drop 执行。因此 wrapper 只保存 `IrqSave::State` 数值，并显式调用
`BaseGuard::acquire/release`。

TrapHandler 对 vsched2 队列的操作仍保持关中断，避免同 CPU IRQ 在持有
`trap_wait_queue` 的普通 `spin::Mutex` 时重入。只在调用 StarryOS dispatcher 的区间打开
中断，dispatcher 返回后立即保存状态并关中断：

```text
vsched2 trap/ready 队列管理：IRQ disabled
StarryOS syscall/IRQ dispatcher：IRQ enabled
返回 vsched2 handler：IRQ disabled
```

### 4.5 根协程被 IRQ 打断时的 continuation

根协程执行期间被 IRQ 打断时，StarryOS 在进入 vsched2 `trap_entry()` 前：

1. `take_current_stack()` 取出协程正在使用的真实栈；
2. 将该栈保存为任务 `thread_stack_ptr`；
3. 暂时把任务从协程改为线程，并记录 `resume_to_coroutine`；
4. vsched2 按原有普通 IRQ 协议使用 `sscratch` 中的 trap 栈处理中断；
5. 任务再次被选中后，`run_task()` 先安装保存的线程栈；
6. 仅该分支使用 `restore_and_sret()` 恢复完整 trap frame 和 `SPIE -> SIE`；
7. `sret` 前清除线程栈指针并恢复协程身份，下一次 IRQ 可重复同一协议。

普通内核线程、人工构造的初始 frame 和现有 Yield 仍使用原来的 `restore_and_jump()`/`jr`
路径。曾尝试把所有 `UserTrapFrameKind::Trap` 统一改为 `sret`，会在 tty-reader 启动早期
破坏调度栈并产生 load page fault，因此不能扩大分流范围。

### 4.6 已修复：block_on 线程化后的单核调度栈未脱离

先区分 vsched2 的三个栈所有权槽：

- `current_stack[cpu]`：当前 CPU 正常执行流使用的栈；
- `trap_stack[cpu]`：预先写入 `sscratch`、供下一次 trap 入口立即切换的栈；
- `Task::thread_stack()`：线程 continuation 自己拥有、恢复该任务时重新安装的栈。

假设 handler 协程正常运行时的栈为 `C`，预分配 trap 栈为 `T`。硬件 trap 入口执行
`csrrw sp, sscratch, sp` 后，CPU 已从 `C` 切到 `T`；vsched2 普通 IRQ 路径再分配下一栈
`N`，形成：

```text
中断前：sp=C, current_stack=C, trap_stack/sscratch=T
入口后：sp=T
trap_entry 后：current_stack=T, trap_stack/sscratch=N
                 set_current_stack 返回的 _old=C
```

若根协程被 IRQ 打断，StarryOS 会在进入 `trap_entry()` 前先
`take_current_stack(C)`，把 `C` 存入任务 `thread_stack_ptr`；因此随后
`set_current_stack(T)` 得到 `_old=None` 是有意行为。这条 IRQ continuation 路径本身不是
本次 panic 的来源。

原 panic 发生在同步 `block_on` 主动让权路径：

```text
handler 在 C 上运行，current_stack=C
  -> Future::poll = Pending
  -> Running -> Blocking，AxWaker 发布 Parked
  -> toggle_handler(true) 把上下文类型改为线程
  -> yield trampoline 把 continuation frame 保存到 C
  -> StarryOS yield 入口 take_current_stack(C)
  -> handler.thread_stack_ptr=C，current_stack=None，但物理 sp 仍在 C
  -> raw_thread_entry
  -> CPU_NUM==1，thread_entry 跳过 empty-stack trampoline
  -> Blocking -> Blocked
  -> kschedule 无 Ready 任务，在物理栈 C 上进入 scheduler-root WFI
  -> CURRENT_TASK 已改成等待上下文，但 current_stack 仍是 None
  -> IRQ 进入 T；等待根双栈轮换再次 take_current_stack()
  -> handler.rs:147 panic
```

这里第一次 `take` 成功并把 `C` 交给了阻塞 handler；第二次 `take` 是等待根想取得独立
调度栈时失败。不能把 `None` 吞掉，也不能把 `C` 直接当成 scheduler/trap 栈，因为 handler
被另一 CPU 唤醒后可能同时恢复 `C`。

vsched2 的 `take_current_stack()` 注释明确说明原实现是临时设计：单核下取栈后调度器仍
短暂运行在同一栈上，并假设后续不再操作该栈。新增 WFI 和等待根 IRQ 轮换使这个假设不再
成立。当前修复统一了线程的保存边界，没有新增 VTABLE ABI：

1. `toggle_handler(true)` 只把 block_on 调用者的上下文类型标记为线程，不再提前独立取栈；
2. StarryOS `vsched_yield_entry_stub()` 先保存寄存器 frame；若上下文类型为线程，再统一调用
   `take_current_stack()`，将返回的栈登记到任务 `thread_stack_ptr`；普通线程和由 block_on
   临时形成的线程因此共用同一交接点；
3. vsched2 `thread_entry()` 在 `CPU_NUM > 1` **或当前上下文为线程**时，通过原有
   `get_empty_stack + tep2_trampoline` 切栈。线程已完成栈移交，所以
   `current_stack=None`，`get_empty_stack()` 会分配独立调度栈 `E`；
4. 切到 `E` 后才执行 `thread_entry_phase2()`，提交 `Blocking -> Blocked` 并进入调度循环。
   等待时满足 `sp=E, current_stack=E, trap_stack=T`，IRQ 双栈轮换可安全得到
   `current_stack=T, trap_stack=E`；
5. 任务被唤醒后，`run_task()` 先把 `C` 重新安装为 current stack，随后从已保存 frame 恢复
   block_on continuation；`toggle_handler(false)` 清除线程栈指针并恢复协程身份；
6. TrapHandler 在 trap 队列为空时仍保持协程身份。它的正常 park 表示放弃本轮 poll
   continuation，下次从根协程重新 poll，因此不会错误地走线程栈保存路径。

不能对所有单核上下文无条件重置 `sp`：只有已经移交栈所有权的线程才必须分配 `E`；普通
单核协程仍可复用当前栈。还需单独审计 StarryOS 普通内核线程：当前 `register_task()` 为
`thread_stack_ptr` 新分配一个 `VschedStackImpl`，但初始 frame 的 `sp` 使用 AxTask 自己的
`kernel_stack_top()`，栈对象和实际硬件栈并不一致。这不是本次 handler panic 的直接原因，
当前主动让权已把该 Stack 对象作为 vsched2 的所有权 token 正确脱离 per-CPU 槽，但在启用
多核前仍应明确这一适配语义并审计 IRQ 路径和回收，避免悬空对象或错误回收。

2026-07-26 验证：`make build` 和 `make verify-vsched2` 通过；`make test` 启动交互 shell 后，
TTY block_on 完成一次线程化保存与恢复，`echo STACK_OK` 正常输出，`exit` 后 init 退出并关闭
QEMU；未再出现 `Failed to take current stack`。

2026-07-27 在 vsched2 `5738b48` 上重复上述验证并直接执行 `hello_world`，结果仍通过；说明
本轮 Scheduler opaque 参数和 per-CPU priority 更新没有破坏等待根、线程栈脱离及 wait4
continuation。

### 4.7 两级 Waker 链（最小交互已验证）

tty-reader 作为普通 vsched2 内核线程运行后，它自身的 `block_on()` 必须把 Waker 绑定到
tty-reader 的 vsched2 task，而不是 AxRunQueue。UART 到达后的链路应为：

```text
PLIC/UART IRQ hook
  -> 唤醒 tty-reader 的 AxWaker
  -> tty-reader 进入 kernel ReadyQueue
  -> reader.poll() 搬运字符
  -> poll_rx.wake()
  -> 唤醒 shell read 所在 TrapHandler 的 AxWaker
  -> TrapHandler continuation 恢复并完成 read
```

`LineDiscipline` 已在注册 Waker 前后各执行一次 `reader.poll()`，用于封闭“检查为空”和
“注册 Waker”之间的事件窗口。移植时应保留这一逻辑，不要用 UART 忙轮询替代正式唤醒。
当前已验证 UART 输入能够多次恢复终端读取链上的阻塞 handler，并完成 `echo`、`pwd`、
相对路径 `mkdir`、重定向、`cat` 和单级管道退出链。连续输入、信号、多级管道和长时间空闲
后的完整压力测试仍按下节执行。

### 4.8 WFI 的周期性 timer 唤醒不是忙转

2026-07-28 在 `wait_irqs()` 的 `wfi` 前加入 `warn!` 后，日志约每 10 ms 出现一次：

```text
wait_irqs: waiting for interrupts...
（约 10 ms）
wait_irqs: waiting for interrupts...
```

这符合当前配置，不表示 WFI 失效。StarryOS 的 `TICKS_PER_SEC` 为 100，`axruntime` 每
`NANOS_PER_SEC / 100 = 10 ms` 安装一次 timer deadline。一次空闲周期的实际流程是：

```text
无 Ready 任务
  -> 输出日志并打开本 hart 中断
  -> WFI，CPU 在两次日志之间休眠
  -> 约 10 ms 后 supervisor timer interrupt 到达
  -> 低层入口暂时把 stimecmp 设为 u64::MAX，抑制旧 pending 源
  -> 延迟执行 timer handler，并安装下一个 10 ms deadline
  -> 没有任务被唤醒，再次进入 wait_irqs/WFI
```

WFI 只保证等待某个可唤醒事件，不保证一直等到“业务任务变为 Ready”；timer、external IRQ、
IPI 都可以令它返回，而且 RISC-V 规范允许实现把 WFI 当作 hint。当前日志间隔稳定在 10 ms，
与系统 tick 完全一致，反而说明 QEMU 中的 WFI 确实在等待 timer。若是 pending 未清或 WFI
退化为忙转，通常会看到几乎没有时间间隔的连续输出；后续诊断可同时记录 `scause` 和每秒
WFI/timer/external/IPI 计数来区分。该 `warn!` 是高频测试日志，验证完成后应删除、降为
`trace!` 或限频，避免串口输出反过来扰动调度时序。

如果后续希望空闲 CPU 不被固定 100 Hz tick 唤醒，需要改的是 timer 策略而不是 WFI：实现
tickless idle，根据软件 timer wheel/alarm 中最早的真实 deadline 设置 `stimecmp`，没有
deadline 时才设置为最大值；同时保留时间记账、signal timer 和多核 timer owner 语义。该项
属于能耗/空闲优化，不是当前单核正确性阻塞点。

此前还存在与 WFI 不同层的软件 timer 事件问题：硬件 timer handler 会安装下一次 deadline，
所以 WFI 每 10 ms 正常唤醒；`axtask::on_timer_tick()` 却在 vsched2 激活时整体提前返回，导致
软件 timer wheel 不执行 `check_events()`。2026-07-29 已在 P0-1 中拆分两种职责：始终检查
软件 timer 事件，只跳过 AxRunQueue 的时间片 tick。具体 sleep/timeout 语义仍按 P0-2 分类验证。

### 4.9 后续验收标准

- `make run` 后 BusyBox prompt 可以持续读取键盘输入；
- 输入前空闲任意时间后仍能立即响应；
- 连续执行多条命令和退格/回车不会丢字符或重复字符；
- 所有用户任务和 handler 阻塞时 CPU 进入 WFI，不进行 UART 忙轮询；
- tty-reader 只属于 vsched2，不同时存在于 AxRunQueue；
- UART external interrupt 能完成 PLIC claim 和 complete；
- tty-reader Waker 和 shell handler Waker 各只造成一次 ReadyQueue 入队；
- wait4 的既有 continuation 验证仍通过；
- `make verify-vsched2` 不出现 vDSO panic、ReadyQueue 满、重复 owner、非法状态或丢失唤醒。

建议交互测试序列：

```sh
echo VSCHED_TTY_OK
pwd
mkdir -p /tmp/vsched-tty
echo hello >/tmp/vsched-tty/a
cat /tmp/vsched-tty/a
echo pipe-ok | cat
cd /tmp/vsched-tty
ls
```

## 5. 待办优先级（按执行顺序）

优先级含义：P0 是当前主线必须先闭环的正确性问题；P1 是紧随其后的多核与生命周期安全；
P2 是语义完整性、公平性和兼容性；P3 是不阻塞当前功能的长期演进与文档清理。P0 内部严格
按 P0-1、P0-2、P0-3、P0-4 的顺序推进，不再把多核、回收或公平性与它们并列。

### 5.1 P0-1：消除对 AxRunQueue 的运行时依赖（单核主路径已修复）

2026-07-29 已在 StarryOS/axtask 适配层完成，不修改 vsched2 调度接口：

1. axtask 新增通用 external-scheduler ownership hooks。`prepare_vsched2()` 在
   `starry_api::init()` 前注册后，既有 `spawn/spawn_raw/spawn_with_name` API 自动把新内核
   任务交给 StarryOS 适配器；`api`、`axnet`、POSIX 层不需要反向依赖 `starry-core`，任务也
   不会同时进入两个调度器。
2. 启动期任务继续使用 `PENDING_KERNEL_THREADS`，但 scheduler-ready 的发布和 pending drain
   现在与 enqueue 使用同一把锁，封闭多核下“已经 drain 后又追加 pending”的永久滞留窗口。
3. 动态内核任务使用 vsched2 的 `push_task()`，由 `Task::is_kernel()` 明确选择 kernel
   scheduler；不再使用 `push_task_into_current()`，因此即使创建发生在处理用户 TrapInfo
   期间，也不会误入当前用户进程的 ReadyQueue。
4. timer tick 已拆分职责：每次硬件 tick 都执行调度器无关的 `timers::check_events()`，只有
   vsched2 未激活时才执行 AxRunQueue 的 `scheduler_timer_tick()`。这补回 sleep/timeout 的
   软件 Waker 来源，同时不恢复旧调度器时间片。
5. closure 自然返回和显式 `axtask::exit()` 统一走 external exit：发布 exit code、设置 AxTask
   `Exited`、唤醒 joiner、失效旧 Waker generation，再由 vsched2 保存/移交线程栈并调度；
   不再进入 legacy `EXITED_TASKS`、GC task 或 AxRunQueue reschedule。
6. `set_priority()` 在 external 模式下更新当前 vsched2 内核任务的 priority，并校验
   `[HIGHEST_PRIORITY, LOWEST_PRIORITY]`。vsched2 尚无 affinity/migration 接口，因此
   `set_current_affinity()` 明确失败，syscall 返回 `Unsupported`，不再静默修改无效的 AxTask
   metadata 或创建 legacy migration task。
7. 短期仍保留 ArceOS runtime 在进入 StarryOS `main()` 前已经初始化的 AxRunQueue/GC task，
   但 vsched2 接管后，普通 spawn、yield、block_on Waker、timer scheduler tick、exit、priority
   和 affinity 均不再把运行时控制交回旧队列。不能把旧队列中的 TCB 批量搬入 vsched2；其
   中混有不同状态和外部 Waker，运行时搬运会产生重复调度、共享栈或悬空引用。

当前验证：

- `make build` 通过；
- `make verify-vsched2` 通过；
- 验证日志确认 `dev-log-server`、`alarm_task` 和 `tty-reader` 均由 vsched2 接管；
- Welcome、wait4/block_on continuation 和 handler 池既有回归保持通过，未出现 vDSO panic、
  非法状态、栈错误或 ReadyQueue 溢出。

剩余边界不再属于“误入 AxRunQueue”的单核主路径：

- `fb-refresh` 和 `vsock-poll` 已由同一 spawn hook 覆盖，但当前 QEMU 无对应设备，尚未做条件
  设备运行验证；
- `kernel task: spawn -> sleep -> wake -> return -> join`、显式 `axtask::exit()` 的专门功能
  用例仍需加入测试矩阵；实现路径已经统一，本项不再依赖 AxRunQueue；
- sleep/alarm/timeout 可作为 P0-2 的延迟唤醒测试载体；其原有语义与取消竞争不在本项处理；
- remote wake/IPI、timer owner、真正的 affinity/migration 归入 P1 多核；
- `VschedTaskImpl::dealloc()` 和最终内核任务回收仍归入 P1 生命周期。

### 5.2 P0-2：验证并修复通用 `block_on` × vsched2 适配层（次优先）

本项只处理因接入 vsched2 和可复用 TrapHandler 而产生的问题，不再把 StarryOS 原有的
futex、signal、PollSet、timer、pipe EOF 或文件系统语义缺陷直接归入 P0-2。具体资源 Future
只作为触发公共适配路径的测试载体；只有能够证明“同一 Future 在原 AxRunQueue 路径正常，
在 vsched2 适配层发生错误”，或直接观察到 handler 状态、Waker 目标、continuation 栈和
ReadyQueue 入队错误时，才修改本节代码。

当前公共模型保持不变：

```text
TrapHandler/普通内核任务 poll Future -> Pending
  -> AxWaker: Idle -> Parking
  -> 当前 vsched2 任务: Running -> Blocking
  -> AxWaker: Parking -> Parked
  -> 协程调用者按需临时转为线程并保存同步 continuation 栈
  -> vsched2 在上下文安全后提交 Blocking -> Blocked
  -> Waker 将同一 continuation 所有者改为 Ready 并唯一入队
  -> 恢复原栈、原执行身份和原 IRQ 状态后继续 poll
```

2026-07-29 审计确认并已修复以下公共适配问题：

1. **已删除 wait4 的旧双模型。** `TrapTaskWaker`、`WaitPidStep::Pending` 和
   `SyscallOutcome::Pending` 属于旧的“TrapHandler 提前返回、以后重放 syscall”方案。共享
   handler 模型下，`vsched2 current` 是 handler、`trapped task` 是用户任务，旧分支正常不会
   命中；若意外命中还会违反 dispatcher 的 `Complete` 断言。wait4 应恢复为普通同步
   `sys_waitpid() -> block_on(...)`，由当前 handler 保存 continuation，直到 syscall 真正完成。
2. **已改为完整发布 block_on 后端后再激活 vsched2。** 不再先写入
   `VSCHED2_YIELD`，再逐个注册
   `BLOCK_ON_CURRENT/WAKE/STATE/TOGGLE`；多核观察者可能看到 `vsched2_active()==true`，但后端
   尚未安装。应先注册全部 hooks，最后用 `VSCHED2_YIELD` 的 Release store 作为发布点，
   `vsched2_active()` 的 Acquire load 作为获取点。
3. **Waker 已改为在创建时绑定调度后端。** `AxWaker::wake_by_ref()` 不再在 wake 时重新读取全局
   `vsched2_active()`。`block_on` 入口只选择一次 Legacy 或 vsched2 owner；vsched2 owner 必须
   保存真实的当前任务指针和 generation。active 路径缺少 current task 或任一必要 hook 时应
   fail-fast，不能保存空指针、静默丢 wake，或在缺少 toggle 时把协程误当普通线程。

当前状态机和栈切换暂未发现需要重写的逻辑：wake-before-Parking 由 `Notified` token 促成立即
repoll；wake 发生在 `Blocking`、上下文尚未保存时只修改为 `Ready`，由保存方完成唯一入队；
wake 发生在 `Blocked` 后由 Waker 完成 `Ready` 和入队；协程调用者继续使用显式
coroutine -> thread -> coroutine 往返保存同步调用链。除非专项测试失败，不扩大 vsched2 侧修改。

专项验收分两层：

- 用户态 `tests/vsched2_test` 自动覆盖重复 nanosleep/timer 延迟唤醒、同一 continuation 的
  多次 block_on 往返，以及同地址空间线程的 spawn/join。BusyBox 前台启动该程序时，父 shell
  同时进入 wait4；只有测试进程完成、退出且父 handler continuation 被唤醒后，交互提示符才会
  再次出现。测试只判断 syscall 是否按时返回，不据此修复资源自身语义；旧 init 中的
  `VSCHED2_SHELL_WAIT4 single PASS` 仅作为历史日志保留。
- 内核适配层继续覆盖 wake-before-park、wake-during-Blocking、duplicate wake、普通 vsched2
  内核线程 `spawn -> sleep -> wake -> return -> join`、嵌套 block_on 和 handler 复用后的 stale
  generation。测试需断言每个等待周期最多一次 ReadyQueue 入队，并记录 task 指针、generation、
  AxWaker 状态和 TaskState；不在正常构建中保留高频日志。

2026-07-29 运行验证：`vsched2_test` 连续 6 次 `thread::sleep(20ms)` 已通过，测得累计等待约
172--177ms；父 BusyBox wait4 在 child 退出后恢复。传统 `fork()` child 首次恢复曾因 syscall
dispatcher 使用只初始化少量寄存器的 `UserContext::new()`、继承到 `gp=0` 而在首个 syscall
之前访问 `0x194`；现已在 dispatcher 中复制完整 trapped GPR，并由 `fork -> child _exit(0)
-> parent waitpid()` 回归验证。该问题不是 block_on Waker 错误。对于真实非法用户访问，
fatal signal 仍缺少 vsched2 返回路径的交付/退出收敛，独立列为 P0-4。

2026-08-01 对 `echo PIPE_DIAG | cat` 的引用计数和唤醒链诊断进一步确认：写入、Pipe
`PollSet` Waker、`cat` 的二次 read 和父 shell wait4 都按原有 StarryOS 语义工作；异常点是
写进程已经 Exited 后，写端 FD 仍由未回收的 `ProcessData::Scope` 持有。修复退出任务的
`Task::dealloc()` 生命周期后，写端立即析构、`cat` 得到 EOF、shell 返回提示符。因此该问题
不需要改写 Pipe，也不需要给某个 Pipe `block_on` 增加专用分支。

2026-08-01 清理全部临时诊断日志后重新构建并启动 QEMU：启动期 `vsched2_test` 的
user-vDSO、timer、fork/waitpid 均 PASS；交互执行 `echo CLEAN_PIPE | cat` 输出
`CLEAN_PIPE` 后立即返回 shell 提示符，未出现 panic、非法状态或诊断日志残留。

此前非交互 init 的最终验证中，`cargo check --release`（`tests/vsched2_test`）、`make build`、
`make test` 和 `make verify-vsched2` 均通过。当前 init 已改为测试后进入交互 shell，自动验证改为
要求 `VSCHED2_TEST timer/clone_thread/fork/PASS` 和测试结束后的 `starry:~#`，以确认前台 child
退出后 shell 的 wait4 continuation 已恢复；没有 vDSO panic、block_on 非法状态、ReadyQueue
溢出或默认路径 SIGSEGV。

多核原子状态和唯一入队属于本项设计约束；vDSO `CPU_NUM` 配置、副核启动、等待 hart 的 IPI、
affinity、timer owner 和任务迁移仍属于 P1。后续 remote wake 在成功 `push_task()` 后补充唤醒
空闲 hart，不改变本节 block_on 状态机。

### 5.3 P0-3：打通不进入内核的用户态线程切换

#### 2026-08-16 更新：阶段 D 的两个前置边界

远端 vsched2 更新和 `Scheduler::sources` 双地址重构没有改变阶段 A、B、C 的任务 ABI、
任务状态或生命周期。它们使阶段 D 的两个前置条件更加明确，开始真实调用
`raw_uschedule()` 前必须完成以下检查。

**前置边界一：U 模式中断与特权指令。**

`run_coroutine()`、`thread_entry()`、`run_thread()` 和 `wait_for_runnable_task()` 的公共
路径不能在 U 模式执行读取 `sstatus` 的 `assert_disable_irq()`，也不能执行 `wfi` 或
`sret`。K 模式继续保持“进入调度器到恢复任务期间关闭本地中断”的约定；U 模式无任务时
通过 `Context::into_kernel()` 交给内核等待。

U 模式无法关闭 S 模式中断，因此还必须防止以下重入：

```text
用户 Scheduler 持有 ReadyQueue/事件源锁
  -> 同一 CPU 进入 S 模式 trap
  -> TrapHandler/Waker 再次访问该事件源
  -> 死锁或调度状态损坏
```

迁移期优先在 StarryOS 专用 vVAR 中使用按 CPU 的安全点和延迟唤醒记录：trap 入口保存
完整 `UserTrapFrame`，中断期间只记录逻辑任务 key、generation 或 pending 标志，不在用户
调度临界区重新获取同一队列锁；用户 Scheduler 到达安全点后再清理。恢复必须回到原 PC、SP、
寄存器和 continuation，不能重放 syscall 或从 Future 的 `poll()` 开头执行。只有 K 模式可以
调用 `wait_irqs()`。

**前置边界二：用户 Stack VTABLE 与 per-CPU 栈所有权。**

最新 `StackHandler` 的空闲栈池是 per-CPU，`alloc_stack()`、`dealloc_stack()` 和当前栈
操作都带 `cpu_id`。阶段 D 必须同时实现用户调度栈、用户线程栈和用户 Context；不能只补
`UserTask::poll()`。用户态不能解引用 `KERNEL_STACKS`、`sscratch`、`StackVirtImpl` 或
`AxTaskRef`，用户 vVAR 只保存固定布局的用户栈描述和逻辑所有权。

协程 `Pending` 转为可恢复线程时必须保留真实 continuation 和正在使用的栈；跨 CPU 唤醒
前必须先解绑原 CPU，不能让同一栈同时属于两个 CPU。首次进入用户线程时要明确线程栈是否
已经安装到用户 StackHandler，不能把线程栈误放入空闲池。

这两个边界是阶段 D 的准入条件，不是已完成项。阶段 D 应按以下顺序推进：

1. 完成 U 模式特权检查、用户 trap frame 和安全点/延迟唤醒协议；
2. 完成用户 Stack、Context、SMP VTABLE 和 per-CPU 栈所有权；
3. 验证同地址空间协程到协程切换；
4. 验证用户线程主动让权到用户协程；
5. 接入 StarryOS vDSO `sched_yield` 和 libc fallback；
6. 再处理 signal、Trap/同步 syscall、remote wake、IPI、任务迁移和 SMP=2/4。

在真实用户态切换稳定前，以下情况必须回到内核：没有本地 Ready 任务、资源阻塞、待处理
signal、exit/execve、跨地址空间或全局优先级调度、需要多核协调，以及任何任务槽/上下文/栈
所有权校验失败。

#### 当前结论

目前**不能**说明 StarryOS 已经实现了不进入内核的用户线程切换。当前只能说明：

- vDSO 已以用户可执行权限映射，并通过 `AT_SYSINFO_EHDR` 交给用户程序；
- vsched2 已提供 `raw_uschedule/uschedule/utask_schedule`、当前地址空间的
  `USER_SCHEDULER`，同进程存在 Ready 任务时的控制流不会主动调用 `Context::into_kernel()`；
- StarryOS 的内核侧已经可以借助该 Scheduler 调度用户任务。

但是，现有 Linux `sched_yield` 仍执行 `ecall`，进入 `sys_sched_yield()` 和
`axtask::yield_now()`，随后使用 StarryOS 内核 `.text` 中的 yield trampoline。它证明的是
“进入内核后由 vsched2 调度”，不是“线程 A 在 U 模式直接切换到同地址空间的线程 B”。

当前审计入口集中在 `api/src/syscall/task/schedule.rs`、`api/src/syscall/task/clone.rs`、
`api/src/task.rs`、`core/src/vsched/task.rs`、`core/src/vsched/context.rs`、
`core/src/vsched/trap_vector.rs` 和 vsched2 的 `src/main_loop.rs`。

2026-07-29 已修复用户任务 vDSO 基址生命周期：地址空间 clone 同步复制 vDSO 元数据，
`clear()` 清除映射时同时清零元数据，plain fork 安装私有 child vDSO 后更新任务缓存，execve
装载新镜像后也在返回用户调度前更新缓存。创建用户 VTI 时对零基址 fail-fast。日志验证
BusyBox fork、execve 和 `vsched2_test` 自身 fork 的 `aspace_vdso == task_vdso != 0`。这补齐了
`Context::into_user()` 的必要前置条件。P0-3 阶段 B 后，同地址空间线程的内核注册闭环已经完成；
用户态 Task VTABLE、共享任务槽实际存储、协作上下文和用户运行时入口仍未完成。

2026-07-30 已修复 BusyBox 执行 `ls` 时 fork 子进程重映射 vDSO 的
`AxErrorKind::AlreadyExists` panic。根因是 StarryOS 曾按整个 `.so` 文件长度再加一页计算
`VDSO_SIZE=0x28000`，并在初始加载、bootstrap 和 clone 中手工补尾部；但生成 loader 实际按
ELF `PT_LOAD.p_memsz` 映射，当前真实跨度仅为 `0x1e000`。文件尾部的 `.symtab/.strtab` 不属于
运行时映射，`.bss` 已包含在 `p_memsz` 中。BusyBox 的复杂地址布局只够 loader 请求的
`vVAR 0x3000 + vDSO 0x1e000 = 0x21000`，随后追加的错误尾部与已有 VMA 重叠。

修复限定在 StarryOS：`vdso` 适配层从对齐后的 ELF `PT_LOAD` 区间计算运行时跨度，
`MemIf::valloc()` 至少按该完整跨度选址，全局 `VDSO_SIZE` 使用同一结果；同时删除三处人工
尾部映射。fork 仍先删除 child 继承的旧 vVAR/vDSO，再由 `map_so()` 建立 child 私有的可写及
重定位页面，没有退回父子共享可写 vDSO。`make build` 通过；BusyBox `ls /`、`mkdir`、重定向、
`cat` 和管道数据传输正常，`vsched2_test` 的 user-vDSO、timer、fork/waitpid 全部 PASS。
管道输出后提示符不返回不属于本次地址冲突，后续已按第 5.6 节的退出任务生命周期缺口修复。

#### 阶段 B 后尚缺的端到端链路

1. **用户可访问的调度控制块。** 当前 `VschedTaskImpl`、`AxTaskRef`、稳定 trap frame 和
   ReadyQueue 裸指针均为内核对象。需要把内核生命周期/资源对象与用户调度热路径分层，
   提供受约束的共享任务槽，至少包含 state、priority、vsched process id、用户栈描述、
   cooperative context 和 generation。已确定不扩展 vsched2 自身 vVAR，而是在 StarryOS 侧
   新建第二个 `libstarry_vsched.so`，由其独立 vVAR 保存全局共享调度数据。
2. **U 模式安全的 Task 操作。** 用户调度不能调用当前指向 StarryOS 内核函数的 Task
   VTABLE，也不能解引用内核裸指针。应使用用户态独立 VTABLE，或在固定布局的共享调度页上
   使用经过校验的索引和原子操作；`Context::into_kernel()` 仅作为受控 fallback。
3. **用户 cooperative context。** 当前 `restore_and_sret()` 需要写 `sepc/sstatus` 并执行
   `sret`，只能由 S 模式使用。主动 yield 的 U 模式入口必须保存和恢复至少
   `ra/sp/gp/tp/s0-s11`，并保证 `tp`/TLS、用户栈和返回地址随线程正确切换。
4. **稳定的 vDSO 执行入口。** 在 StarryOS 专用 `libstarry_vsched.so` 中提供并导出类似
   `__vdso_sched_yield` 的完整入口：保存当前线程上下文，提交 `Running -> Ready`，放回本进程
   Scheduler，进入 vsched2 已有用户调度循环并恢复另一个线程。两个 `.so` 不依赖 ELF 跨模块
   符号解析，由用户运行时初始化各自 VTABLE 并完成接口连接；不能只裸调当前通用的
   `raw_uschedule`。
5. **用户运行时接入。** 启动代码分别解析 `libvsched2.so` 和 `libstarry_vsched.so` 的基址，
   初始化两个 API VTABLE 及用户侧 Task、Stack、Context、SMP 实现；测试程序及后续 libc/pthread
   优先调用 StarryOS vDSO 入口，入口不可用或不满足本地切换条件时回退普通 syscall。

单页表并不能省略第 2、3、4 项：它只表示用户页表中保留内核映射，不表示 U 模式能够执行
没有 USER 权限的内核 `.text`、读写内核堆或执行 `sret`。

#### yield 状态语义与回内核条件

必须继续区分：

- 资源等待：`Running -> Blocking -> Blocked`，由资源 Waker 恢复；
- 协作让权：`Running -> Ready`，放回当前地址空间的 ReadyQueue 后再选择其它线程。

现有 syscall 形式的 `sched_yield` 还必须让出发起 syscall 的用户任务，而不是把代为处理
syscall 的 handler 变成无 Waker 的 `Blocked`。用户态 vDSO 入口完成后，以下情况仍应进入
内核：本进程没有其它 Ready 线程、等待内核资源、待处理 signal、exit/取消/execve、需要跨
地址空间或全局优先级调度、调度元数据校验失败，以及多核 remote wake/affinity/迁核需要
内核协调。目标是提供同地址空间的本地快速路径，而不是删除内核调度入口。

#### 已完成基础工作

**基线与 vDSO 映射记录。** 本阶段没有启用用户态本地切换，没有修改 syscall、
trap、任务状态或 vsched2 调度逻辑，也不增加参与运行时决策的计数和全局状态。只完成：

1. 用户测试通过 `AT_SYSINFO_EHDR` 确认 vDSO 已映射，并只读校验 ELF magic；
2. 构建回归检查现有 vDSO 导出的 `raw_thread_entry/raw_run_task/raw_trap_entry` 以及
   Task、Stack、Context、TrapInfo、SMP、VSpace、UserData 的 VTABLE 初始化入口；
3. 冻结当前边界：这些是 vsched2 已有的通用入口，不等于 StarryOS 已经具有稳定的用户调度
   ABI；当前 `.so` 没有可供 libc 直接调用的 `__vdso_sched_yield`，用户 Task VTABLE、共享
   控制块和 cooperative context 也尚未定义；
4. 当前标准 `sched_yield()` 仍是 ecall 路径。阶段 A 只通过代码审计记录这一事实，不主动
   调用或改写它，避免把尚未完成的 syscall-yield 适配混入 ABI 基线。

阶段 A 的检查不得改变 BusyBox、wait4/block_on、fork/exec、Hello World 或现有调度行为。
后续共享结构只在出现真实消费者时定义，并按照 `vdso_crate_template` 的 `extern "C"`、VTABLE
和 `#[repr(C)]` 规则形成 ABI；不预先增加统一 header 或猜测布局。

2026-07-30 阶段 A 已完成：没有新增 `metrics` 模块，没有修改 StarryOS syscall/trap/任务状态
或 vsched2。`vsched2_test` 只读确认用户 vDSO 基址非零且 ELF magic 正确，实际输出
`VSCHED2_TEST user_vdso PASS base=0x7a000`；`make verify-vsched2` 同时确认上述通用入口仍存在，
并保持 timer、fork/waitpid、BusyBox wait4、init 退出及既有 panic/非法状态检查全部通过。宿主
已有 QEMU 占用了默认 5555 端口并锁定 `arceos/disk.img`，因此运行验证使用关闭网络的 `/tmp`
镜像副本；同一内核和用户测试内容正常完成，环境冲突没有通过修改项目配置规避。

2026-08-08 阶段 A 的双 vDSO 补充验证已完成：StarryOS 侧新增的
`libstarry_vsched.so` 与原有 `libvsched2.so` 均由现有 `build_vdso` 生成、独立映射并通过
各自 loader 初始化；`AT_SYSINFO_EHDR` 继续指向 vsched2，`AT_SYSINFO` 指向 StarryOS 专用
vDSO。阶段 A 的独立 vVAR 原子字段已完成用户读写以及 fork 子进程重新初始化 VTABLE、读取并
原子增加、父进程复读的验证；`make build`、用户程序构建、测试镜像复制和现有 vsched2 日志
回归无新增失败，独立磁盘 QEMU 启动也通过；默认 `make test` 在本机仍可能被已有
QEMU 占用 UDP `5555` 或 `arceos/disk.img` 锁阻断，这是宿主环境限制，不是代码回归。本阶段仍
未把共享字段接入调度决策，也未实现用户态 Task VTABLE 或 cooperative context。

主工程已经不再构建或链接旧的 `vqueue`/`libvqueue`：已删除其 loader 构建、Cargo 依赖、未启用
的 `core/src/vipc.rs` 和旧生成目录。仓库中的 `tests/vipc_test` 暂时保留，因为它是独立的外部
`vipc` 回归工程，并通过绝对路径使用该工程自己的 vqueue；它不属于 StarryOS 主内核依赖，若
后续确认不再维护 IPC 回归，再单独删除该测试及外部工程。当前 `tests/Makefile` 已将其从默认
用户程序集合排除，`make test` 不会再触发这条废弃依赖链。

**进程 ID 与线程注册记录。** 建立独立的 `no_std` 公共类型 crate，修复
`CLONE_THREAD -> vsched_process_id -> USER_SCHEDULER` 注册闭环；在双 vDSO 基础上增加共享任务
句柄和内核 registry，但不改变现有 vsched2 调度队列中的任务指针类型。

2026-08-01 阶段 B 已完成：

1. 新增工作区 crate `vsched_abi`，当前只定义独立的 `VschedProcessId` 命名空间及其保留值。
   `vdso` loader 的普通 Rust 重导出不会形成 `.so` ABI，因此不再保留该依赖。后续实际的 vDSO
   函数使用 `extern "C"` 导出，跨边界结构使用 `#[repr(C)]`，并由 `build_vdso` 生成 API crate、
   VTABLE 和符号表；
2. `ProcessData` 持久保存 `process_init()` 返回值。Linux pid/tid 不再被当成
   `PROCESS_INFO_TABLE` 索引；init、fork、execve 和最后线程退出分别完成绑定、替换和唯一注销；
3. `CLONE_THREAD` 共享父 `ProcessData`，继承同一 `VschedProcessId`，并显式推入父地址空间已有的
   `USER_SCHEDULER`。用户测试 `std::thread::spawn -> join` 验证子线程执行、退出、
   `clear_child_tid` futex 唤醒和父线程恢复；
4. 测试同时暴露并修复一个 StarryOS 适配问题：`FUTEX_WAKE` 的 AxRunQueue 公平性
   `yield_now()` 在 vsched2 handler 中会把未提交 Ready 的 handler 从 `Running` 变为
   `Blocked`，使 pthread 无法继续进入 `exit/clear_child_tid`。现在该公平性让权仅在
   AxRunQueue 模式执行；vsched2 模式由 handler 完成 syscall 后自然回到调度器；
5. StarryOS 的 yield trampoline 在调用 vsched2 `raw_thread_entry` 前使用现有 `IrqSave`
   强制关本地中断，满足所有调度入口的明确约定。没有增加全局变量，没有修改 vsched2 源码。

阶段 B 只证明“内核能够把同地址空间线程正确登记到同一个用户 Scheduler”，不证明线程已经
在 U 模式直接切换。用户 VTABLE、cooperative context、`__vdso_sched_yield` 和 libc 接入仍属于
后续阶段；这些接口在出现实际消费者时再冻结具体布局。

2026-08-08 共享任务 ABI 补充工作已完成并通过验证：

1. `vsched_abi` 新增 `UserTaskKey(slot, generation)`、`SharedTaskSlot` 和固定容量
   `SharedTaskTable`。槽位通过 `FREE -> RESERVED -> LIVE` 原子协议分配，释放时先推进
   generation、清空用户可见投影，再发布 `FREE`，旧句柄不能命中复用后的槽位；所有槽索引均
   做边界检查，`stack_base/stack_size/context/wake_cpu` 目前只保留布局，不参与调度；
2. `libstarry_vsched.so` 的共享 vVAR 保存任务表，内核通过物理页取得同一份表；用户能够写入
   的 process、状态和 context 字段不作为内核身份依据；
3. StarryOS 内核建立 `UserTaskKey -> VschedTaskImpl -> AxTaskRef` registry。初始用户任务、
   私有 fork、`CLONE_THREAD`、execve 和任务 dealloc 都会注册、替换或使 key/generation 失效；
   `CLONE_THREAD` 先注册共享槽再进入 ready queue，避免多核下子线程先运行退出的竞态；key 查询
   在 registry 锁内复制 `AxTaskRef`，避免与退出回收并发解引用已释放的任务对象；
4. 阶段 B 没有把共享槽接入 `USER_SCHEDULER`，没有修改 vsched2 Task VTABLE 或调度逻辑。

验证结果：`cargo test -p vsched-abi`（2 个单元测试通过）、`make build` 成功；`make test` 已
启动完整镜像并输出 `VSCHED2_TEST user_vdso PASS`、`starry_vdso PASS`、`timer PASS`、
`clone_thread PASS`、`FORK PASS`、`VSCHED2_TEST PASS`，随后按交互式 `init.sh` 进入 shell。
QEMU 由测试完成后手动终止，终止不是测试失败。多核测试仍按 TODO 的独立 CPU_NUM/副核启动
问题排期，阶段 B 只完成了多核注册顺序的静态竞态修复。

#### 设计约束与历史记录

StarryOS 双 vDSO、用户 VTABLE 与 cooperative context 遵守以下边界：

- 原则上不修改 vsched2；现有 Task、Stack、Context、SMP 接口及 `uschedule/utask_schedule` 逻辑
  继续承载用户态实现。只有发现与 vsched2 自身设计直接相关的通用缺陷时，才进行最小的上游
  修改。2026-08-14 的 EventSource 双地址、双函数入口属于该例外：原有相对 Scheduler 偏移
  无法表示位于独立共享映射中的事件源；
- 不修改 `vdso_crate_template`，直接复用现有 `build_vdso`、`vdso_helper::vvar_data!` 和生成
  loader；
- 所有 StarryOS 专用任务表示、AxTask 绑定、用户 context、用户栈描述、双 `.so` 生命周期和
  用户运行时连接均在 StarryOS 侧实现；
- `libstarry_vsched.so` 的 vVAR 是内核与所有用户地址空间共享的同一组物理页。跨核或跨地址
  空间并发字段必须使用原子操作或锁；共享“指针值”不代表该指针能在所有地址空间解引用，
  持久身份统一使用 slot + generation，不向用户暴露 `AxTaskRef` 或内核裸指针。

**双 `.so` 构建、加载和全局 vVAR 记录。**

1. 在 StarryOS 侧新建 `starry_vsched_vdso` 源 crate，由现有 `build_vdso` 生成
   `libstarry_vsched.so` 和对应 API/loader crate；先只在独立 vVAR 中定义一个测试用
   `AtomicUsize`，不接入调度决策；
2. 内核启动时依次映射并初始化 `libvsched2.so`、`libstarry_vsched.so`；用户地址空间创建时也
   映射两者。`AT_SYSINFO_EHDR` 继续表示主 `libvsched2.so`，短期可使用当前未占用的
   `AT_SYSINFO` 传递第二个基址，后续再决定是否采用固定地址或模块描述表；
3. `AddrSpace` 至少记录两个模块的 base/size，`clear()`、非 `CLONE_VM` fork、execve 和私有
   `.data/.bss` 重映射必须同时处理两个模块；`CLONE_VM` 线程继续共享同一地址空间中的两份
   映射；
4. 生成 loader 的 `MemIf` 方法签名相同，先验证现有 StarryOS `MemImpl` 能否同时服务两个
   `map_so()`；不要增加第二个同名 `crate_interface` 实现以免产生重复链接符号。若实际链接
   验证失败，只在 StarryOS loader 侧增加第二模块的映射适配，不修改模板；
5. 验收顺序为：内核写、用户进程 A 读写、内核复读、用户进程 B 读取，再覆盖 fork/exec；所有
   观察者必须看到同一 vVAR 值，且 BusyBox、wait4/block_on、Pipe、clone 和原 vDSO 路径无回退。

**共享任务 ABI 与生命周期记录。**

1. 在 `vsched_abi` 中按实际消费者增加 `UserTaskKey(slot, generation)`、`SharedTaskSlot`、
   `SharedTaskTable` 和必要的状态/栈/context 基础布局，不增加统一 `AbiHeader`；
2. `libstarry_vsched.so` vVAR 保存全局 task slot、分配器、进程归属和后续 remote-wake 元数据。
   所有索引做边界检查，slot 回收先使旧 generation 失效，用户写入的数据不能被内核无条件信任；
3. StarryOS 内核保留 `UserTaskKey -> VschedTaskImpl -> AxTaskRef` 注册表。共享槽只保存用户调度
   所需的稳定投影，不搬入文件表、信号内部对象、页表、内核栈或其他内核资源；
4. 本阶段只验证注册、查询、回收、generation 和 fork/exec/exit 生命周期，不把共享槽放入
   `USER_SCHEDULER`。

**StarryOS 用户任务指针适配审计。**

1. 用户任务进入 `USER_SCHEDULER` 时，不直接把 `SharedTaskSlot*` 伪装成
   `VschedTaskImpl*`。vsched2 队列继续保存 opaque task pointer，StarryOS 提供带有
   `UserTaskKey(slot, generation)` 的 `TaskAdapter`，由用户 VTABLE 和内核 VTABLE 分别解析；
2. 内核侧现有 Task VTABLE 通过 StarryOS 分发适配器区分内核任务和用户任务：内核任务继续解析为
   `VschedTaskImpl`，用户任务通过稳定 TaskKey 查找内核 registry。`current_task_ptr()` 的调用点
   不再无条件强转为 `VschedTaskImpl`，统一经过域相关的解析函数；
3. 覆盖 trap、context、clone/exec、退出和 Waker 路径，保证内核处理用户 task 指针时处于正确
   地址空间或完成明确的地址翻译；
4. 本阶段仍不启用用户态 yield，先从内核侧重复 push/pop 用户 Scheduler，验证状态、优先级、
   pid 和生命周期与现有行为一致。

2026-08-10 用户任务指针适配审计记录：

1. 审计了 StarryOS 中所有 vsched2 opaque task pointer 的创建、传递、
   强转和回收路径，确认当前 `USER_SCHEDULER`、TrapInfo、Context、Waker 和 trap 入口保存的
   都是 `VschedTaskImpl` 指针；`SharedTaskSlot` 只存在于共享 vVAR，不能直接作为现有 Task
   VTABLE 的对象指针。
2. 在 `core/src/vsched/task.rs` 增加限定作用域的 typed resolver。它
   只解析来自 vsched2 Task VTABLE 的 `VschedTaskImpl`，并通过闭包阻止借用引用逃逸；共享
   `UserTaskKey` 另通过 registry 锁保护的 resolver 查询。block_on、任务退出、内核线程入口、
   trap/yield、Context 和 clone 检查路径已经统一使用这些入口，未改变调度状态或队列算法。
3. 使用标准 `make verify-vsched2` 完成构建和 QEMU 日志复核，
   覆盖初始用户任务、私有 fork、`CLONE_THREAD`、wait4/Waker、TrapHandler pool、BusyBox 提示符
   和 init 退出。验证输出包含 `user_vdso PASS`、`timer PASS`、`clone_thread PASS`、`FORK PASS`
   和最终 `PASS`，未出现 vDSO panic、非法任务状态或 ready queue 溢出。每个任务仍使用独立的
   `VschedTaskImpl`，同地址空间线程只共享 vsched2 process id；Linux pid/tid 和用户可写槽字段
   均未作为 opaque task pointer。
4. **暂停边界**：在不修改 vsched2 Task VTABLE/opaque pointer ABI 的前提下，不能安全地把
   `USER_SCHEDULER` 中的指针替换为 `SharedTaskSlot`。vsched2 会直接按 `TaskVirtImpl` 调用
   Task VTABLE；下一步若用户态 VTABLE 确实要求槽指针，必须先设计完整的 StarryOS Task
   adapter 和独立解析路径，再决定是否需要最小的 vsched2 通用接口调整，不能在当前阶段强行
   混用两种布局。

**2026-08-11 方案重整：LogicalTask 双域视图。**

P0-3 的目标不是复制完整的 `VschedTaskImpl` 到用户态，也不是让内核和用户各自维护一份
可独立修改的任务状态，而是让同一个逻辑任务拥有两个执行视图：

```text
LogicalTask(UserTaskKey)
  ├─ SharedTaskSlot：公共身份、task_state、context_kind、priority、进程 ID、generation
  ├─ KernelTaskView：VschedTaskImpl、AxTaskRef、TrapFrame、内核栈和内核资源
  └─ UserTaskView：用户寄存器上下文、用户栈、协程 continuation
```

必须遵守以下约束：

1. `SharedTaskSlot::state` 只表示槽位生命周期；新增的 `task_state` 才表示
   `Ready/Running/Blocking/Blocked/Exited`，不能混用；
2. `context_kind` 表示最新保存的是用户协程、用户线程还是 Trap 上下文；
   `context_owner` 和 `queue_owner` 使用带校验的逻辑令牌，不保存内核裸指针；
3. 公共字段由共享 vVAR 作为唯一权威来源，`AxTaskState`、`VschedTaskImpl` 和用户对象只能
   通过原子状态协议同步，不能形成两个互相独立的调度状态机；
4. 用户栈和上下文字段只保存所属地址空间可解释的描述符、索引或用户虚拟地址。内核访问时
   必须经过地址翻译，不能直接解引用用户地址；
5. 用户任务进入 `USER_SCHEDULER` 时使用 `TaskAdapter/UserTaskView` opaque 指针，内核任务
   仍使用 `VschedTaskImpl*`。只有在确认通用 handle 解析确实需要时，才讨论对 vsched2 增加最小
   接口，不修改其调度算法、队列策略或状态语义；
6. `libvsched2.so` 继续提供相同的调度算法代码，用户 VTABLE 和内核 VTABLE 分别绑定各自域
   的实现；`libstarry_vsched.so` 只提供 StarryOS 专用的用户 ABI、TaskAdapter 和上下文入口。

#### 新的单层实施计划

- **阶段 A（已完成）：基线与双 vDSO。** 冻结现有 syscall/trap/vsched2 行为；完成
  `libvsched2.so` 与 `libstarry_vsched.so` 的构建、映射、fork/exec 生命周期和共享 vVAR 验证，
  同时移除主工程不再使用的 vqueue。
- **阶段 B（已完成并通过验证）：共享 ABI 与任务生命周期。** 建立 `VschedProcessId`、
  `UserTaskKey(slot, generation)`、共享任务槽、内核 registry、独立任务状态和上下文/队列所有权
  协议；完成 init、fork、`CLONE_THREAD`、execve 和 exit 的注册闭环，但不接入现有
  `USER_SCHEDULER`。
- **阶段 C（已完成并通过验证）：任务 ID 与用户 VTABLE。** 用户调度器保存编码后的
  `UserTaskKey(slot, generation)`，内核校验共享槽后通过 registry 解析到 `VschedTaskImpl`；用户侧
  Task VTABLE 已建立，Stack、Context、SMP 和真实 `raw_uschedule` 仍留在阶段 D。
- **阶段 D（待实施）：协作上下文与本地切换。** 先完成 U 模式特权检查、用户 trap frame、
  安全点/延迟唤醒协议，以及用户 Stack、Context、SMP VTABLE 和 per-CPU 栈所有权；再保存和
  恢复 `ra/sp/gp/tp/s0-s11` 及用户栈，依次验证协程到协程、线程主动让权到协程。无本地 Ready
  任务、资源阻塞、信号、退出、校验失败或跨地址空间时通过 `Context::into_kernel()` 回退。
- **阶段 E（待实施）：Trap、libc 与多核收敛。** 完成 Trap/同步 syscall 的 Thread/Trap 上下文
  与用户 continuation 交接，使 `sched_yield()` 优先走 vDSO 并保留 syscall fallback；随后验证
  fork/exec、signal、block_on、remote wake、IPI、任务迁移和 SMP=2/4，并在用户上下文稳定后
  接入真实外部 EventSource。

2026-08-11 阶段 B 的共享状态契约已完成并通过验证：

1. `SharedTaskSlot` 增加独立的 `task_state`、`context_kind`、`context_owner` 和 `queue_owner`，
   槽位生命周期 `state` 不再与 vsched2 任务状态混用；
2. `vsched_abi` 增加 generation 校验的任务状态 CAS、上下文初始发布、上下文所有权和就绪队列
   所有权接口。所有权只接受逻辑令牌，不接受内核裸指针；
3. `register_user_task()` 在任务进入现有 ready queue 前发布初始线程/协程类型，任务释放时由
   generation 失效协议清除状态和所有权；本次没有把共享槽接入 `USER_SCHEDULER`，没有改变
   vsched2 的 Task VTABLE、状态转换或队列算法；
4. `cargo test -p vsched-abi` 通过 3 项单元测试，覆盖保留进程 ID、槽位回收后的旧 generation
   失效，以及 task_state、上下文/队列所有权和上下文发布协议；
5. `make build` 和 `make verify-vsched2` 通过。后者完成 vDSO 符号检查和单核 QEMU 日志复核，
   覆盖初始任务、fork、`CLONE_THREAD`、execve/exit、BusyBox、wait4/block_on 和现有 vDSO
   回归，未接入阶段 C 的用户态调度路径。

2026-08-13 阶段 C 已完成并通过验证：

1. `UserTaskKey(slot, generation)` 使用最低标记位编码为不可解引用的任务 ID。用户任务进入
   `USER_SCHEDULER` 前使用该 ID，内核任务继续使用原有 `VschedTaskImpl*`，没有修改 vsched2
   的队列布局、调度算法或接口；
2. 内核 Task VTABLE 改为 StarryOS 的 `TaskImpl` 分发层。编码 ID 先校验共享槽的 generation，
   再在 registry 锁保护下解析到 `VschedTaskImpl`；直接内核任务保持原有指针路径。可能让权、
   恢复上下文或回收的操作不会带着 registry 锁进入非局部控制流；
3. 新增 `no_std` `vsched_user`，在用户地址空间的私有 vsched2 `.data/.bss` 中安装 `UserTask`
   VTABLE。状态、优先级、上下文类型和 vsched2 process id 均直接访问 StarryOS vVAR 的同一份
   `SharedTaskTable`，不存在内核态和用户态各自维护一套调度状态；
4. `libstarry_vsched.so` 新增 `user_task_table()`，只暴露共享表地址，不暴露 `AxTaskRef`、
   `VschedTaskImpl*` 或其他内核对象。槽位生命周期和任务调度状态继续分离，旧 generation
   不能解析到复用后的任务；
5. 用户回归会初始化两个 vDSO API VTABLE 和用户 Task VTABLE，解析 LIVE 槽位并验证状态、
   `match_set_state`、优先级、上下文类型和进程 ID；日志输出
   `VSCHED2_TEST user_task_vtable PASS`。测试锁文件将间接构建依赖 `x86_64` 固定为仓库其余
   组件已使用的 `0.15.4`，避免当前 nightly 与 `0.15.5` 的 `Step` 接口不兼容；
6. `cargo test -p vsched-abi` 通过 4 项单元测试，`cargo check -p vsched-user`、RISC-V musl
   用户程序构建、`make build` 和 `make verify-vsched2` 均通过。完整 QEMU 回归同时覆盖
   fork/clone、wait4/block_on、TrapHandler 池、BusyBox 提示符和既有 vDSO 路径，未出现
   vDSO panic、非法状态或 ready queue 溢出；
7. 阶段 C 不调用用户态 `current_task_ptr()`、`raw_uschedule()`，也不执行 U 模式上下文切换。
   这些入口需要尚未安装的用户 Stack、Context、SMP VTABLE 以及 cooperative context；为验证
   Task 动态分派而提前调用它们会混入阶段 D。阶段 C 的验收边界是内核动态 VTABLE 已实际使用
   编码 ID 完成现有调度回归，用户 Task VTABLE 已安装并验证所有无上下文副作用的方法。

2026-08-14 完成 EventSource 双视图基础设施：

1. `Scheduler::sources` 不再保存事件源相对 Scheduler 的偏移，而是为每项缓存
   `kernel_data`、`user_data` 和 `EventSourceVtable`。内嵌的 ReadyQueue、TrapWaitQueue 仍通过
   字段偏移计算其用户地址；独立共享映射中的外部事件源可以提供不相关的 B/C 地址。调度热路径
   根据当前 `IN_KERNEL` 选择地址，不查询页表；
2. `EventSourceVtable` 分别保存内核态和用户态的 `hightest_priority`、`take_task` 入口。
   vsched2 内建事件源在进程初始化时把内核 vDSO 函数 A 转为用户函数 B；实现代码位于其它
   vDSO 的外部事件源可以通过 `EventSourceVtable::new()` 显式提供两套入口；
3. `UserData` 新增仅供初始化和注册路径使用的 `get_user_addr(A/C -> B)`。StarryOS 对 vsched2
   vDSO、vVAR 使用段内偏移转换，对独立共享映射按物理页反查目标用户地址，并拒绝不连续或存在
   多个不同用户地址的映射；
4. vsched2 远端 `2921170` 主要把空闲栈池改为 per-CPU，并延续此前的中断关闭、栈轮换和一致性
   检查。本次保持这些实现，不把 EventSource 地址转换混入栈和 trap 路径；
5. `cargo check`、两轮 `make build` 和 `make test` 通过。自动测试输出 user-vDSO、用户 Task
   VTABLE、StarryOS vDSO、timer、clone thread、fork/wait 全部 PASS，并进入 BusyBox。宿主机
   仍有其它 QEMU 占用 `disk.img` 和 5555 端口，因此未在第二个实例重复交互命令测试；
6. 本轮不新增公开事件源注册 ABI。原有 `Scheduler::register_event_source` 已支持双数据地址和显式
   双入口 VTABLE，但仍为内部方法；待出现第一个真实外部源后，再按其生命周期、目标进程和权限
   约束设计公开注册/注销接口，避免提前冻结一组裸指针 ABI。

### 5.4 P0-4：接入用户 trap 返回前的信号交付与退出收敛

本项本质是 **vsched2 与 StarryOS 信号机制的集成问题**，不是 signal pending 队列本身损坏。
无法修复的页错误已经能把 `SIGSEGV` 放入正确用户进程的 pending 集合，
`ProcessSignalManager::send_signal()` 也会返回可唤醒 tid；但当前
`vsched_trap_dispatcher()` 随后只调用 `AxTask::interrupt()` 并返回。`interrupt()` 只设置
axtask 的 interrupted 标志并唤醒其 Waker，不会消费 pending signal、执行默认动作或修改
用户 trap frame。

受控非法写验证始终观察到同一 task、同一 `sepc`、同一 `stval` 和正确页表根；从第二次起
`SIGSEGV pending_before=true`，同时持续为 `wake_tid=Some(...)`、`pending_exit=false`。因此
`raise_signal_fatal_for_task()` 的直接 `do_exit()` fallback 不会命中，handler 返回后 vsched2
又恢复原 faulting frame，形成：

```text
用户故障 -> SIGSEGV pending -> AxTask::interrupt()
         -> TrapHandler 返回 -> 原 sepc 恢复 -> 同一用户故障
```

修复必须复用普通 StarryOS 用户循环的信号语义，不能把所有 SIGSEGV 简化成无条件
`do_exit()`，否则会破坏用户自定义 signal handler：

1. 抽取“返回用户态前交付 pending signal”的公共函数，普通用户循环和 vsched2 dispatcher
   使用同一语义，包括 `unblock_next_signal()` 与 `check_signals()`；
2. 始终以真正被 trap 的用户任务为 signal owner，不能把可复用 TrapHandler 当作进程线程；
3. signal handler 修改了 `sepc/sp` 或 signal frame 时，把完整 `UserContext` 写回稳定
   `UserTrapFrame`，不能只写 `a0/a1`；
4. 默认 Terminate/CoreDump/Stop 动作调用 `do_exit()` 后，TrapInfo 完成路径必须保留 `Exited`，
   禁止重新入 ReadyQueue 或恢复 faulting frame；
5. 保持 block/ignore、用户 handler、`sigreturn`、syscall restart 和 group-exit 的原有语义，
   并明确多线程进程中被选中 tid 与实际 trapped task 的关系。

验收用例至少包括：未安装 handler 的 SIGSEGV 只退出一次且 parent wait4 收敛；安装用户
SIGSEGV handler 后能进入 handler；被阻塞/忽略 signal 遵循既有策略；fatal signal 不重复刷屏；
正常 syscall、page fault 按需填页、timer/block_on 和 fork 回归不倒退。

### 5.5 P1：多核配置、启动和跨核唤醒

2026-07-27 在最新 vsched2 `main` 上复测：`make SMP=4 build` 成功，OpenSBI 和 StarryOS 均
识别 4 个 hart，但 vsched2 vDSO 的 `CPU_NUM` 仍为 1，hart 1/2/3 访问 `CURRENT_TASK` 等数组
时越界；另有副核在 `VschedTaskImpl::execution_task()` 访问无效 execution context 时页错误。
必须先修复数组长度，才能判断后者是否为独立问题。

先闭合 `SMP -> CPU_NUM -> vdso_helper::mut_cfg!` 的配置传播和 Cargo 缓存失效规则，确保
StarryOS、vsched2 `.so`、`libvsched2` wrapper 及所有 per-CPU 数组使用同一数值；随后验证
secondary bootstrap、per-hart `stvec/sscratch/gp`、current/trap stack、timer owner、IPI、
remote wake、WFI、affinity、任务迁移、共享 handler 池和 AxWaker 唯一入队。

### 5.6 P1：任务、handler、栈和 process slot 生命周期

- **普通退出任务的基本回收已完成。** `sys_exit` 仍只提交 Exited，不在 syscall 内过早释放；
  TrapHandler 在 TrapInfo 处理完成、确认任务为 Exited 且不会再次入队后调用
  `Task::dealloc()`。StarryOS 实现会先失效 Waker generation，释放稳定 trap frame 和线程
  Stack 对象，在最后一个进程线程退出时注销借用 AddrSpace 裸指针的 process slot，最后释放
  `VschedTaskImpl -> AxTaskRef -> Thread -> ProcessData::Scope`。`echo PIPE_OK | cat` 已验证 FD
  table 随 Scope 回收、写端析构、读端 EOF 和父进程 wait4 的完整链路；
- 增加 handler 总数、历史高水位、软阈值日志和明确硬上限；池耗尽不能静默忙等，因为待
  处理 TrapInfo 可能正是解除资源阻塞的中断；
- `VschedTaskImpl::dealloc()` 已不再是空操作，并对状态、动态 execution owner 和 trap owner
  做 fail-fast 校验。仍需用专门的内核任务 `spawn -> sleep -> wake -> return -> join`、显式
  exit、stale Waker 和多核迁移用例，确认对象退出前已经离开 current、ReadyQueue、所有资源
  等待队列和 continuation；
- 审计普通内核线程的 vsched Stack 所有权 token 与 AxTask 实际硬件栈不一致的问题，明确
  IRQ、主动 yield、跨核恢复和最终回收的所有权；
- `VSpace::dealloc()` 当前采用借用式所有权而为 no-op；若改为拥有式句柄必须成对释放。

### 5.7 P2：调度语义和系统调用兼容性

- 同优先级进程当前偏向 current process，不是公平轮转；后续使用 per-CPU cursor，只在当前
  最高优先级集合内轮转，并定义进程注销、优先级变化和多核同步语义；
- 用户态中断路径仍有上游 `todo!("用户态中断处理流程")`。当前硬件中断作为内核调度上下文
  处理；用户态快速切换完成后需明确被 IRQ 抢占、signal 注入和重新进入用户 Scheduler 的
  边界；
- `mkdir -p /tmp/...` 会在处理根目录 `/` 时返回 `EINVAL`，而相对路径 mkdir 正常；应作为
  文件系统 syscall 语义问题独立诊断，不与 pipe/block_on 合并；
- `UserData::get_user_data()` 仍兼容上游把 small pid 当作 vspace 参数的实际行为，应在接口
  语义稳定后移除这种双义兼容；
- vDSO 构建图仍有一次产物时序窗口：`vdso/build.rs` 生成新的 `libvsched2.so`，而生成的
  `libvsched2` loader crate 又通过 `include_bytes!` 嵌入该文件；同一次 Cargo 构建中 loader
  可能先于新 `.so` 完成编译。2026-08-01 清理临时日志时曾观察到首轮镜像仍嵌入上一版
  `.so`，第二轮构建后才同步。后续应把 `.so` 生成改为明确的前置阶段/依赖产物，并在
  `make verify-vsched2` 中校验嵌入镜像与新生成 `.so` 的一致性，避免旧 vDSO 造成假阳性验证。

### 5.8 P3：长期演进与文档清理

- 将逐类 syscall/Future 迁移为 vsched2 可见的协程执行流；同步 syscall 仍保持一次激活快速
  完成，最终再评估 io_uring 式异步系统调用提交/完成队列；
- `block_on设计简述.md`、`my_block_on设计文档.md` 和 drawio 中仍有已撤销的独立
  SyscallTask 描述，应同步成当前“可复用 TrapHandler + 叶子 Future continuation”模型；
- 功能稳定后清理或降级高频诊断日志，只保留队列高水位、stale Waker、非法状态和回退原因
  等可运维计数。

## 6. 验证矩阵

| 范围 | 命令/方式 | 当前状态 |
|---|---|---|
| 最新 vsched2 编译/ABI | `make build`、`make verify-vsched2` | ✅ 本轮在当前 vsched2 working tree 通过 |
| 单核启动与 wait4 接力 | `make verify-vsched2` | ✅ 通过 |
| Welcome/Hello 里程碑 | 交互 shell 直接执行 `hello_world` | ✅ 均正常输出 |
| init `exit`/系统关机 | shell `exit` + 父 init wait4 | ✅ QEMU 正常关闭 |
| BusyBox prompt/UART 输入 | 交互版 `src/init.sh` + `make test` | ✅ 多轮输入，无栈 panic |
| 基础命令/文件 I/O | `echo`、`pwd`、相对 `mkdir`、重定向、`cat` | ✅ 已通过最小回归 |
| 绝对路径 `mkdir -p` | `mkdir -p /tmp/vsched-regression` | ❌ 根目录分量返回 `EINVAL` |
| 管道退出链 | `echo PIPE_OK \| cat` | ✅ 写端随 Exited task dealloc 析构，`cat` EOF，shell wait4 后返回提示符 |
| timer event 来源 | `on_timer_tick` ownership 审计 | ✅ 始终 `check_events`，只跳过 AxRunQueue tick |
| block_on × vsched2 公共适配 | `vsched2_test` + BusyBox wait4 + handler/内核任务专项 | ⚠️ timer、单 wait4 和 fork/waitpid 通过；内核竞态专项待补 |
| 传统 `fork()` child 恢复 | Rust FFI `fork` 后 child `_exit(0)`、parent `waitpid()` | ✅ dispatcher 复制完整 GPR，child `gp` 与退出链正确 |
| fork/exec vDSO 基址同步 | clone、exec 日志对照 `AddrSpace` 与 `VschedTaskImpl` | ✅ 两者非零且相等；clone/clear 元数据生命周期已补齐 |
| fork 子进程 vDSO 私有重映射 | BusyBox `ls /` + `vsched2_test` fork/waitpid | ✅ PT_LOAD 运行时跨度统一为 `0x1e000`，不再追加文件尾部保留区 |
| fatal 用户 signal 收敛 | 故意 SIGSEGV + 默认动作/用户 handler | ❌ pending 可入队但未在 vsched2 返回路径交付；P0-4 |
| 同地址空间线程注册 | `std::thread::spawn -> join` | ✅ 共用 `VschedProcessId/USER_SCHEDULER`，退出 futex 闭环通过 |
| 同地址空间 U 模式切换 | 双线程 vDSO yield + 原因分类计数 | ❌ 未打通：仍经 ecall；共享任务槽、用户 VTABLE 和 U 模式上下文尚缺 |
| 双核 | `SMP=2 make test` | ⏳ 阶段 4 |
| 四核静态构建 | `make build SMP=4` | ✅ 通过 |
| 四核短时启动 | `timeout 15s make justrun SMP=4` | ❌ vDSO CPU_NUM 仍为 1，副核数组越界/页错误 |

当前 `make verify-vsched2` 的关键日志为：

```text
Welcome to Starry OS!
[vsched2] kernel task accepted: name=dev-log-server ...
[vsched2] kernel task accepted: name=alarm_task ...
[vsched2] kernel task accepted: name=tty-reader ...
sys_waitpid <= pid: ...
[block_on] coroutine -> thread task=...
trap handler pool grow: handler=..., cpu=...
[block_on] thread -> coroutine task=...
VSCHED2_TEST timer PASS
VSCHED2_TEST clone_thread PASS
VSCHED2_TEST FORK PASS pid=...
VSCHED2_TEST PASS
starry:~#
vsched2 log verification passed
```

测试程序作为前台 child 完成后才出现 shell 提示符，因此这条回归仍覆盖 wait4 continuation；
当前交互 init 不再输出旧的 `VSCHED2_SHELL_WAIT4/VSCHED2_INIT_TEST` 专用标记。

## 7. 历史问题索引

| 问题 | 状态 |
|---|---|
| `pop_task()` 未写回剩余最高优先级 | ✅ 当前 vsched2 已修复 |
| 资源阻塞 handler 又被 TrapWaitQueue 取出 | ✅ 只有全局 idle 池中的 handler 可复用 |
| 每处理一个 TrapInfo 都归还 handler | ✅ 当前 CPU 队列为空前持续处理 |
| wait4 阻塞唯一 trap handler | ✅ continuation + 共享 handler 池 |
| wait4 Waker 唤醒用户任务并 replay ecall | ✅ 唤醒 handler；旧 replay 有 current guard |
| Pending frame 被共享 `TF_POOL` 覆盖 | ✅ 每任务稳定 frame |
| handler yield 泄漏 trap-frame Box | ✅ 原位复用稳定 frame |
| 嵌套 handler trap 找不到用户 owner | ✅ 有界 `trap_owner` 链 |
| execve 后用户 Scheduler 未初始化 | ✅ process_init/user_init/process_drop 已接入 |
| `is_kernel()` 错误依赖 `pid == 0` | ✅ 特权级与进程/地址空间身份解耦 |
| 普通内核线程缺少 axtask current | ✅ external-current 桥和初始 frame |
| `TrapInfo::handle()` 忽略 `Some/None` | ✅ 任务 trap 与外部中断语义已区分 |
| StarryOS 侧 vsched per-CPU 数组硬编码为 1 | ✅ 使用 `axconfig::plat::CPU_NUM` |
| vsched2 vDSO 的 CPU_NUM 未跟随 `SMP` | ❌ 当前四核启动直接阻塞点 |
| 无 Ready 任务时关中断忙转 | ✅ scheduler-root 发布、复查、WFI |
| scheduler-root IRQ 每次泄漏一个 trap 栈 | ✅ 只在等待根使用双栈轮换 |
| init 用户任务退出后 QEMU 不关闭 | ✅ 登记 init vsched task，提交 Exited 后 system_off |
| block_on 取走 continuation 栈后单核仍在该栈调度 | ✅ 统一 yield 栈移交；线程在 phase2 前切换 empty stack |
| 普通内核线程的 vsched Stack 与 AxTask 实际栈不一致 | ⚠️ 已按所有权 token 脱离；多核前仍需审计 IRQ 和回收语义 |
| 内核根协程 poll 的 IRQ 状态丢失 | ✅ 每协程 `IrqCorotineWrapper` |
| 根协程被 IRQ 打断后 continuation 无法恢复 | ✅ 临时线程 + `sret` + 恢复协程身份 |
| 后台任务和通用 spawn 仍进入 AxRunQueue | ✅ external spawn hook；启动 pending 与动态内核任务统一进入 vsched2 |
| timer Future 事件随 AxRunQueue tick 一起跳过 | ✅ 始终检查 timer event，仅旧 scheduler tick 被禁用 |
| 内核任务自然返回/显式 exit 进入旧调度器 | ✅ external exit 发布 exit/join 后由 vsched2 调度 |
| priority/affinity 回落 AxRunQueue | ✅ priority 映射；affinity 未实现时明确失败 |
| `config.log=true` 仍需改 vsched2 | ✅ template 自动日志桥 |
| vDSO panic 无输出 | ✅ template panic 日志 |
| `make test` 复制整个 target 导致 ENOSPC | ✅ 只复制 release 顶层程序 |
| fork/exec 后 `user_vdso_base == 0` | ✅ clone/clear 元数据与 task 缓存同步，创建期非零校验 |
| BusyBox `ls` fork 时扩展 vDSO 返回 `AlreadyExists` | ✅ StarryOS 按 ELF PT_LOAD/p_memsz 统一运行时跨度，删除三处人工尾部映射 |
| Pipe 数据输出后 `cat` 不 EOF、shell 不返回 | ✅ Exited Trap task 在完成后调用 `Task::dealloc()`，StarryOS 回收 Scope/FD table |
| `CLONE_THREAD` 未进入父进程 `USER_SCHEDULER` | ✅ 持久保存 `VschedProcessId`，共享线程显式进入同一 Scheduler |
| pthread join 卡在 `clear_child_tid` | ✅ vsched2 模式跳过 `FUTEX_WAKE` 的 AxRunQueue 公平性 yield，handler 可继续执行 exit |
| fatal SIGSEGV 重复恢复同一 `sepc` | ❌ P0-4：pending 已入队，vsched2 返回路径尚未交付/退出收敛 |
| `Welcome to Starry OS!` | ✅ 当前自动验证通过 |
| `Hello, World!` | ✅ 旧 init 脚本已通过 |
