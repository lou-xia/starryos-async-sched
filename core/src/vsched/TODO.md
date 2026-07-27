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

扩展测试发现两个尚未纳入已通过基线的 syscall/文件系统边界：`echo PIPE_OK | cat` 能输出
`PIPE_OK`，但 shell 随后不能回到提示符，Ctrl-C 也不能完整收敛；`mkdir -p /tmp/...` 会因
尝试创建 `/` 得到 `EINVAL`，相对路径 `mkdir` 正常。它们与第 5 节“其它 block_on 调用点”及
文件系统语义的既有未完成范围一致；本轮没有为此扩大 vsched2 修改。

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
相对路径 `mkdir`、重定向和 `cat`。管道可传输数据但结束/等待链不能收敛；连续输入、信号和
长时间空闲后的完整压力测试仍按下节执行。

### 4.8 后续验收标准

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
按 5.1、5.2、5.3 的顺序推进，不再把多核、回收或公平性与它们并列。

### 5.1 P0-1：消除对 AxRunQueue 的运行时依赖（最优先）

tty-reader 已迁入 vsched2，但其它后台任务和通用任务 API 仍可能只把任务放进 AxRunQueue：

| 任务/接口 | 影响 |
|---|---|
| `dev-log-server` | `/dev/log` 服务不运行 |
| `alarm_task` | alarm 和 signal timer 不推进 |
| `fb-refresh` | framebuffer 刷新不运行 |
| `vsock-poll` | vsock 轮询不运行 |
| ArceOS/POSIX spawn/join/exit | 内核任务创建、等待与退出协议尚未统一 |
| AxRunQueue gc/idle/migration | 生命周期、SMP 和 affinity 仍属于旧调度器 |

不能简单遍历 AxRunQueue 并把任务指针塞入 vsched2。队列中混有 Ready、Running、Blocked、
Exited 和正在切换的任务，其 Waker 还可能挂在 IRQ、timer、WaitQueue、pipe、socket 或
joiner 上。只搬 TCB 会造成重复调度、永久阻塞、共享栈或悬空引用。

实施与验收要求：

1. 审计全部 `spawn/spawn_raw/spawn_with_name`、join/exit 和直接访问 AxRunQueue 的路径；
2. 新内核任务统一通过 kernel-spawn hook 进入 vsched2；初始化前必须保留的任务进入显式
   pending 列表，初始化后只入 vsched2 ReadyQueue；
3. 分别补齐这些任务的 Waker、阻塞、退出、join 和回收协议，不能让同一任务同时属于两个
   调度器；
4. 用 vsched2 的等待根、生命周期和多核机制替代旧 scheduler 的 idle/gc/migration 责任；
5. vsched2 激活后，不得存在“已被唤醒但只在 AxRunQueue 中、因而永远不会运行”的必要任务。

### 5.2 P0-2：逐类验证并修复 `block_on` 调用（次优先）

wait4 和终端 read 的最小 continuation/Waker 闭环已经验证，但这不能证明其它 Future 正确。
需要依次覆盖 futex、sleep/timer、pipe、文件/磁盘、poll/select/epoll、signal、eventfd、网络、
WaitQueue 和 mutex：

- 2026-07-27 已命中 pipe 未完成用例：`echo PIPE_OK | cat` 数据能够输出，但 shell 的
  pipe EOF/子进程退出/wait4 链不能收敛；需分别记录两个子进程的 FD 引用、exit 状态、对应
  handler continuation 和 Waker generation，定位是写端未关闭、退出/wait4 关系错误还是
  唤醒丢失；
- 注册 Waker 后必须二次检查等待条件，封闭检查与注册之间的丢失唤醒窗口；
- 正常完成、signal、timeout、取消和资源关闭并发时只能完成及入队一次；
- continuation 恢复后不能重复已经发生的 I/O 副作用；
- exit/execve 必须使遗留 Waker 失效，不能唤醒已注销或复用后的任务；
- 普通内核线程和 TrapHandler 两类 `block_on` 调用者都必须满足状态、栈和 IRQ 协议；
- 多核 remote wake 必须有正确的内存顺序、目标 CPU 通知和唯一入队保证。

共享 handler 池只解决“一个 syscall 阻塞时其他 handler 仍能处理后续 trap”，不自动证明
具体资源 Future 或其取消路径正确。每类调用都要有可重复的功能测试、signal/timeout 测试和
Waker 计数断言。

### 5.3 P0-3：打通不进入内核的用户态线程切换

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

#### 尚缺的端到端链路

1. **同地址空间线程注册。** 新地址空间执行 `process_init()` 后，要把返回的
   `vsched_process_id` 持久保存在 `ProcessData` 或等价的地址空间对象中；`CLONE_THREAD`
   必须继承该 id，设置新线程的 vsched2 pid，并把线程推入父进程的同一个
   `USER_SCHEDULER`。不能混用 Linux pid/tid 和 `PROCESS_INFO_TABLE` 索引。
2. **用户可访问的调度控制块。** 当前 `VschedTaskImpl`、`AxTaskRef`、稳定 trap frame 和
   ReadyQueue 裸指针均为内核对象。需要把内核生命周期/资源对象与用户调度热路径分层，
   提供受约束的 `UserSchedTask`，至少包含 state、priority、vsched process id、用户栈、
   cooperative context、generation/cookie，并放在每进程用户可访问的调度页中。
3. **U 模式安全的 Task 操作。** 用户调度不能调用当前指向 StarryOS 内核函数的 Task
   VTABLE，也不能解引用内核裸指针。应使用用户态独立 VTABLE，或在固定布局的共享调度页上
   使用经过校验的索引和原子操作；`Context::into_kernel()` 仅作为受控 fallback。
4. **用户 cooperative context。** 当前 `restore_and_sret()` 需要写 `sepc/sstatus` 并执行
   `sret`，只能由 S 模式使用。主动 yield 的 U 模式入口必须保存和恢复至少
   `ra/sp/gp/tp/s0-s11`，并保证 `tp`/TLS、用户栈和返回地址随线程正确切换。
5. **稳定的 vDSO ABI。** 在 vDSO 中提供并导出类似 `__vdso_sched_yield` 的完整入口：保存
   当前线程上下文，提交 `Running -> Ready`，放回本进程 Scheduler，进入用户调度循环并恢复
   另一个线程。不能只导出并裸调当前仍为局部符号的 `raw_uschedule`。
6. **用户运行时接入。** 由启动代码从 `AT_SYSINFO_EHDR` 解析入口，测试程序及后续
   libc/pthread 优先调用 vDSO；入口不可用或不满足本地切换条件时回退普通 syscall。

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

#### 建议实现与验收顺序

1. 修复 `CLONE_THREAD -> vsched_process_id -> USER_SCHEDULER` 注册闭环；
2. 设计每进程 `UserSchedTask`/用户调度页和安全句柄，保留独立的 `KernelTask` 生命周期；
3. 实现 U 模式 Task 操作和 cooperative context；
4. 实现、导出并接入 `__vdso_sched_yield`，无本地任务时可靠 fallback；
5. 加入 `vdso_local_switch_count`、`vsched_into_kernel_count`、`sched_yield_ecall_count`，并按
   cause 细分 trap 计数，避免把异步 timer/IRQ 误算成 yield 主动进入内核；
6. 用同地址空间线程 A/B 循环 yield 验证：二者属于同一 `USER_SCHEDULER`、页表根不变、
   本地切换计数增长、`into_kernel` 和 `sched_yield` ecall 计数不增长，且栈、`tp`/TLS 和
   返回地址正确；测试期间发生的异步中断单独记录，不作为用户切换主动陷入；
7. 最后验证 signal、退出、无本地 Ready 任务以及多核 remote wake 等回退边界。

### 5.4 P1：多核配置、启动和跨核唤醒

2026-07-27 在最新 vsched2 `main` 上复测：`make SMP=4 build` 成功，OpenSBI 和 StarryOS 均
识别 4 个 hart，但 vsched2 vDSO 的 `CPU_NUM` 仍为 1，hart 1/2/3 访问 `CURRENT_TASK` 等数组
时越界；另有副核在 `VschedTaskImpl::execution_task()` 访问无效 execution context 时页错误。
必须先修复数组长度，才能判断后者是否为独立问题。

先闭合 `SMP -> CPU_NUM -> vdso_helper::mut_cfg!` 的配置传播和 Cargo 缓存失效规则，确保
StarryOS、vsched2 `.so`、`libvsched2` wrapper 及所有 per-CPU 数组使用同一数值；随后验证
secondary bootstrap、per-hart `stvec/sscratch/gp`、current/trap stack、timer owner、IPI、
remote wake、WFI、affinity、任务迁移、共享 handler 池和 AxWaker 唯一入队。

### 5.5 P1：任务、handler、栈和 process slot 生命周期

- 普通进程需在 TrapInfo 完成且确认不会再次入队后 deferred 回收 task、稳定 frame、
  vsched2 process slot、旧 vDSO 和地址空间引用；不能在 `sys_exit` 内过早
  `process_drop()`；
- 增加 handler 总数、历史高水位、软阈值日志和明确硬上限；池耗尽不能静默忙等，因为待
  处理 TrapInfo 可能正是解除资源阻塞的中断；
- `VschedTaskImpl::dealloc()` 对内核任务仍为 no-op。回收前必须失效 Waker generation，并
  确认对象已经离开 current、ReadyQueue、资源等待队列和 continuation；
- 审计普通内核线程的 vsched Stack 所有权 token 与 AxTask 实际硬件栈不一致的问题，明确
  IRQ、主动 yield、跨核恢复和最终回收的所有权；
- `VSpace::dealloc()` 当前采用借用式所有权而为 no-op；若改为拥有式句柄必须成对释放。

### 5.6 P2：调度语义和系统调用兼容性

- 同优先级进程当前偏向 current process，不是公平轮转；后续使用 per-CPU cursor，只在当前
  最高优先级集合内轮转，并定义进程注销、优先级变化和多核同步语义；
- 用户态中断路径仍有上游 `todo!("用户态中断处理流程")`。当前硬件中断作为内核调度上下文
  处理；用户态快速切换完成后需明确被 IRQ 抢占、signal 注入和重新进入用户 Scheduler 的
  边界；
- `mkdir -p /tmp/...` 会在处理根目录 `/` 时返回 `EINVAL`，而相对路径 mkdir 正常；应作为
  文件系统 syscall 语义问题独立诊断，不与 pipe/block_on 合并；
- `UserData::get_user_data()` 仍兼容上游把 small pid 当作 vspace 参数的实际行为，应在接口
  语义稳定后移除这种双义兼容。

### 5.7 P3：长期演进与文档清理

- 将逐类 syscall/Future 迁移为 vsched2 可见的协程执行流；同步 syscall 仍保持一次激活快速
  完成，最终再评估 io_uring 式异步系统调用提交/完成队列；
- `block_on设计简述.md`、`my_block_on设计文档.md` 和 drawio 中仍有已撤销的独立
  SyscallTask 描述，应同步成当前“可复用 TrapHandler + 叶子 Future continuation”模型；
- 功能稳定后清理或降级高频诊断日志，只保留队列高水位、stale Waker、非法状态和回退原因
  等可运维计数。

## 6. 验证矩阵

| 范围 | 命令/方式 | 当前状态 |
|---|---|---|
| 最新 vsched2 编译/ABI | `make build`、`make verify-vsched2` | ✅ `5738b48` 通过 |
| 单核启动与 wait4 接力 | `make verify-vsched2` | ✅ 通过 |
| Welcome/Hello 里程碑 | 交互 shell 直接执行 `hello_world` | ✅ 均正常输出 |
| init `exit`/系统关机 | shell `exit` + 父 init wait4 | ✅ QEMU 正常关闭 |
| BusyBox prompt/UART 输入 | 交互版 `src/init.sh` + `make test` | ✅ 多轮输入，无栈 panic |
| 基础命令/文件 I/O | `echo`、`pwd`、相对 `mkdir`、重定向、`cat` | ✅ 已通过最小回归 |
| 绝对路径 `mkdir -p` | `mkdir -p /tmp/vsched-regression` | ❌ 根目录分量返回 `EINVAL` |
| 管道退出链 | `echo PIPE_OK \| cat` | ❌ 数据输出后 shell 不返回 |
| timer/sleep/timeout | 分类测试 | ⏳ 中断恢复后验证 |
| 同地址空间 U 模式切换 | 双线程 vDSO yield + 原因分类计数 | ❌ 未打通：仍经 ecall，线程注册和 U 模式上下文尚缺 |
| 双核 | `SMP=2 make test` | ⏳ 阶段 4 |
| 四核静态构建 | `make build SMP=4` | ✅ 通过 |
| 四核短时启动 | `timeout 15s make justrun SMP=4` | ❌ vDSO CPU_NUM 仍为 1，副核数组越界/页错误 |

当前 `make verify-vsched2` 的关键日志为：

```text
Welcome to Starry OS!
[wait4] BLOCK children=...
[block_on] coroutine -> thread task=...
trap handler pool grow: handler=..., cpu=...
[block_on] thread -> coroutine task=...
vsched2 log verification passed
```

在修复输入时必须保持这条回归用例，防止为了终端唤醒破坏 wait4 continuation。

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
| `config.log=true` 仍需改 vsched2 | ✅ template 自动日志桥 |
| vDSO panic 无输出 | ✅ template panic 日志 |
| `make test` 复制整个 target 导致 ENOSPC | ✅ 只复制 release 顶层程序 |
| `Welcome to Starry OS!` | ✅ 当前自动验证通过 |
| `Hello, World!` | ✅ 旧 init 脚本已通过 |
