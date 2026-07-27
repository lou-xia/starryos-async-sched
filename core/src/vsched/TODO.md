# vsched2 移植到 StarryOS：状态、路线与 TODO

> 平台：RISC-V 64、QEMU virt。先稳定单核，再启用多核。
>
> 当前基线：StarryOS `26f7e08`；vsched2 `585b86d`。
>
> 架构和文件索引见 `ARCHITECTURE.md`。本文只记录当前有效设计、待办事项和验收标准；
> 已撤销的 SyscallTask/`TrapHandlerResult` 试验不再作为实现基础。

## 1. 当前基线（2026-07-24）

### 1.1 已完成

- `Scheduler::pop_task()` 的最高优先级写回问题已由当前 vsched2 修复；
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
- `make verify-vsched2` 当前通过单核自动日志验证。

### 1.2 当前直接阻塞点

`src/init.sh` 现在直接进入 BusyBox login shell。系统能打印：

```text
starry:~#
```

随后 shell 的 `read(0, ...)` 使 handler 进入 `block_on Pending`，但 UART 输入不能完成从
硬件中断到该 handler continuation 的唤醒闭环。因此当前最优先任务是恢复 BusyBox 交互
输入，而不是继续扩展 block_on 或修改 vsched2 API。

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

### 阶段 1：修复 BusyBox 交互输入

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

### 阶段 3：加入用户态 vDSO 并验证 `utask_schedule()`

先用读取时间等无状态函数验证用户程序可以解析并调用 vDSO，且没有发生 ecall；再加入
同地址空间用户线程切换入口。

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

## 4. P0：BusyBox 输入阻塞修复方案

### 4.1 已确认现象与旧 init.sh 能运行的原因

当前日志停在：

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

### 4.2 三个必要断点

#### 断点 A：中断 dispatcher 丢弃了全部中断

`api/src/task.rs::vsched_trap_dispatcher()` 当前在检测到 `scause` 最高位为 1 后直接返回。
因此 supervisor external interrupt 虽然能形成 `TrapInfo(None)`，却没有调用
`axhal::irq::irq_handler()`：

- PLIC 不会 claim UART IRQ；
- UART handler/IRQ hook 不运行；
- PLIC 不会 complete；
- `register_irq_waker()` 中的 PollSet 不会 wake。

RISC-V `axplat-riscv64-qemu-virt` 的 IRQ 接口要求传入完整 `scause`，包括最高位。正式实现
不能只传 `scause & mask`；否则 `S_TIMER/S_SOFT/S_EXT` 分类会失效。

#### 断点 B：tty-reader 仍在旧 AxRunQueue

`api/src/terminal/ldisc.rs` 通过 `axtask::spawn_with_name(..., "tty-reader")` 创建后台线程。
它在 vsched2 接管前已经被放入 AxRunQueue；接管后 `yield_now()` 和 `block_on()` 虽已重定向
到 vsched2，但旧 AxRunQueue 不再选择该任务。

即使 UART IRQ hook 成功 wake，它也只会回到旧队列，不能读取字符并唤醒 shell 的 read。

#### 断点 C：没有可被 IRQ 唤醒的 vsched2 idle 边界

当用户任务、handler 和 tty-reader 都阻塞后，vsched2 的 `kschedule()` 会在关中断状态下
循环寻找任务。没有 Ready 任务时 CPU 忙转，SIE 保持关闭，已经 pending 的 UART IRQ 也
无法进入 trap vector。

此外，当前 S 模式 `Trap` frame 的 `restore_and_jump()` 使用 `jr` 恢复执行，没有恢复保存
的 `sstatus.SIE/SPIE`。因此内核任务从一次 IRQ 恢复后可能继续关中断运行。idle 至少必须
在每次 `WFI` 前显式开中断；后续还要统一 S 模式 trap 的正确返回语义。

### 4.3 最小实现设计

#### 步骤 0：加入阶段性、限频诊断日志

只记录事件和任务状态，不打印用户输入内容：

- 收到的完整 interrupt `scause`；
- `axhal::irq::irq_handler()` 是否处理成功；
- PLIC 返回的 UART IRQ 编号；
- tty-reader 的 `Blocked/Blocking -> Ready`；
- tty-reader 成功读取的字节数；
- shell read handler 的 Waker generation、状态迁移和 read 返回长度；
- idle 的进入、IRQ 唤醒和再次让权计数。

这些日志用于定位链路断在哪一跳；验证稳定后降为 `trace` 或删除高频输出。

#### 步骤 1：恢复统一的硬件中断分发

修改 StarryOS dispatcher 的 interrupt 分支：

1. 保留 `TrapInfo(None)`，不能制造用户任务 owner；
2. 把完整 `tf.scause` 交给 `axhal::irq::irq_handler(tf.scause)`；
3. external interrupt 由平台层完成 PLIC claim、设备 handler/IRQ hook 和 complete；
4. timer interrupt 由现有 axruntime timer handler 重装下一次 deadline；
5. software interrupt 留给后续 IPI/remote wake，但现在不能静默吞掉。

低层 stub 当前在 supervisor timer 进入时暂时把 `stimecmp` 设为最大值，正式分发后必须确认
它只用于阻止重复进入；下一次 deadline 仍由 axruntime 注册的 timer handler 唯一重装，
不能形成两套 timer owner。

`axtask::on_timer_tick()` 在 vsched2 激活后目前直接返回。后续应拆分为：

- 仍推进 timer wheel、到期 callback 和 Future Waker；
- 只跳过旧 AxRunQueue 的时间片调度。

这不是 UART 单字符输入的最小前置条件，但属于阶段 1 的中断正确性验收，否则 sleep、超时
和信号定时器仍会永久阻塞。

#### 步骤 2：加入 vsched2 管理的 idle 内核线程

单核阶段在 `kernel_init_main()` 完成后、第一次进入 vsched2 调度前创建一个 idle 线程：

- 普通 vsched2 内核线程，而不是 TrapHandler；
- `is_kernel=true`、`pid=0`、最低优先级；
- 始终有一个可运行的 idle，使 `kschedule()` 不再在“完全无任务”时关中断空转；
- 每轮先显式 `enable_irqs()`，再执行 `WFI`；从 IRQ 返回后关闭中断/进入合适 guard，再通过
  vsched2 cooperative yield 重新让权；
- idle 不进入旧 AxRunQueue，也不参与普通资源等待队列。

必须避免“检查无任务”和“开中断等待”之间的丢失唤醒：RISC-V pending IRQ 在 SIE 打开后
应立即触发，idle 不能先清 pending 再 WFI。

多核阶段扩展为每 CPU 一个本地 idle 线程；idle 本身不可跨核，普通 handler 仍可跨核。

S 模式 IRQ 的长期正确方案是为 `UserTrapFrameKind::Trap` 增加原子恢复 `sepc/sstatus` 的
S-mode 返回路径，优先使用标准 `sret` 语义，避免在寄存器和栈尚未恢复时提前打开中断。
在该路径完成前，idle 每轮显式开中断是必要保护，但不能被当作所有内核任务的最终修复。

#### 步骤 3：提供最小的 vsched2 kernel-spawn 桥

不迁移 AxRunQueue 中的任意 TCB，也不让同一任务同时属于两个调度器。采用一个窄接口：

1. 在 `axtask::spawn_raw/spawn_with_name` 增加“外部内核调度器 spawn hook”；未注册 hook 时
   完全保持原行为；
2. 选择 vsched2 的启动路径时，在 `starry_api::init()` 和首次触发 `N_TTY` 之前注册 hook；
3. hook 使用 `new_raw()` 创建 AxTaskRef，但不加入 AxRunQueue；
4. vsched2 内核 scheduler 尚未初始化时，将 AxTaskRef 放入显式 pending 列表；
5. `kernel_init_main()` 完成后，把 pending 任务逐个包装为普通 vsched2 内核线程：
   `is_kernel=true, pid=0, coroutine=None`，再推入 kernel ReadyQueue；
6. scheduler 已 ready 后的新后台任务直接走同一包装和入队路径；
7. `spawn_task(TaskInner)` 等用户任务创建路径暂不被这个 hook 接管，避免和现有 clone/
   process scheduler 注册发生双重入队。

这样可以先迁入 tty-reader，同时复用于 `dev-log-server`、`alarm_task`、`fb-refresh` 和
`vsock-poll`。第一轮只以 tty-reader 为验收对象，其他任务按功能阶段逐个验证。

选择 hook 而不是在 `ldisc.rs` 直接依赖 vsched2 的原因是：终端代码只表达“创建一个内核
后台任务”，不应知道具体调度器；也可以避免为每个后台服务复制 register/push 逻辑。

#### 步骤 4：验证两级 Waker 链

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

### 4.4 实施顺序和停点

按以下顺序提交小步改动：

1. 中断分类/分发与限频日志；
2. 单核 idle 线程及 SIE/WFI 边界；
3. kernel-spawn hook、pending 列表和 tty-reader 迁移；
4. 验证 UART -> tty-reader -> read handler 全链路；
5. 拆分 timer wheel 推进与旧 AxRunQueue tick；
6. 清理诊断日志并开始基础命令验证。

每一步只修改 StarryOS；若发现必须修改 vsched2 ABI、需要从 AxRunQueue 强行搬运活动任务，
或出现与当前状态协议不同的新问题，应停止扩大修改，先记录日志和原因再重新评估。

### 4.5 阶段 1 验收标准

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

## 5. 其它待办问题

### P0：仍依赖 AxRunQueue 的后台任务和 API

除 tty-reader 外，已知还有：

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

处理原则：新任务统一走 kernel-spawn hook；接管前创建但确实需要保留的任务使用显式
pending 列表；旧 scheduler 的 idle/gc/migration 由 vsched2 对应机制替代。

### P0：其它 block_on 调用点尚未验证

当前只验证了 wait4。futex、sleep、pipe、文件/磁盘、poll、signal、eventfd、终端、网络、
WaitQueue 和 mutex 等需要逐类检查：

- 注册 Waker 后是否二次检查条件；
- 正常完成、signal、timeout 和取消并发时是否只完成一次；
- 恢复后是否重复产生 I/O 副作用；
- exit/execve 是否取消遗留 Waker；
- 普通内核线程和 TrapHandler 两种调用者是否都满足状态协议；
- SMP remote wake 是否只入队一次。

共享 handler 池只解决“一个 syscall 阻塞不妨碍后续 trap”，不自动证明叶子 Future 正确。

### P0：普通 `yield_now()` 语义

必须区分：

- 资源等待：`Running -> Blocking -> Blocked`，由资源 Waker 恢复；
- 协作让权：先提交 `Running -> Ready`，再进入 vsched2 trampoline，由 ReadyQueue 重调度。

handler 内若直接 yield 却最终成为无 Waker 的 `Blocked`，会永久持有当前 TrapInfo。用户
`sched_yield` 还要决定它让出的是发起 syscall 的用户任务，而不是让 handler 自身失联。
本问题不与 BusyBox 输入修复混改。

### P0/P1：handler 池容量和回收

- 增加全局 handler 总数和历史高水位；
- 增加软阈值日志和明确硬上限；硬上限不能简单等于 CPU 数；
- 池耗尽时不能静默忙等，因为待处理 TrapInfo 可能正是解除资源阻塞的中断；
- `VschedTaskImpl::dealloc()` 对内核任务仍为 no-op；Exited handler、栈、根协程、AxTaskRef
  和旧 Waker 需要 deferred reclaim；
- 回收前必须失效 generation，并确认对象已离开 current、ReadyQueue、资源等待队列和
  continuation。

### P1：init 退出和系统生命周期

`vsched2_bootstrap() -> !` 永久进入调度循环，init 用户任务退出后 `main` 的 SBI shutdown
不可达。需要统一定义：init completion、剩余内核任务、无任务 idle/shutdown、handler 和
栈的回收。

### P1：用户态 `utask_schedule()`

当前 Linux `sched_yield` 仍通过 ecall 进入内核。同地址空间用户线程尚未真正使用 U 模式
vDSO 快速切换，所需工作已列在阶段 3。

### P2：调度公平性和资源所有权

- 同优先级进程当前偏向 current process，不是公平轮转；它不是输入/wait4 的正确性前提，
  但在交互与负载测试前需要设计公平策略；
- 用户态中断路径仍有上游 `todo!("用户态中断处理流程")`，当前 StarryOS 适配把硬件中断
  作为内核调度上下文处理；进入真正 U 模式调度前要重新确认边界；
- process slot、用户任务、稳定 trap frame、execve 旧 vDSO 和地址空间需要统一回收；
- `VSpace::dealloc()` 当前采用借用式所有权所以为 no-op；改成拥有式句柄时必须成对释放；
- `UserData::get_user_data()` 仍兼容上游把 small pid 当作 vspace 参数的实际行为；
- `block_on设计简述.md`、`my_block_on设计文档.md` 和 drawio 仍含已撤销的独立 SyscallTask
  描述，待输入链稳定后同步改成可复用 TrapHandler 模型。

## 6. 验证矩阵

| 范围 | 命令/方式 | 当前状态 |
|---|---|---|
| vsched2 编译 | `cargo check`（外部库） | ✅ 通过 |
| 单核启动与 wait4 接力 | `make verify-vsched2` | ✅ 通过 |
| BusyBox prompt | `make run` | ✅ 能显示 |
| BusyBox UART 输入 | 手工交互及基础命令序列 | ❌ 当前 P0 |
| timer/sleep/timeout | 分类测试 | ⏳ 中断恢复后验证 |
| 同地址空间 U 模式切换 | 双线程 yield + ecall 计数 | ⏳ 阶段 3 |
| 双核 | `SMP=2 make test` | ⏳ 阶段 4 |
| 四核 | `SMP=4 make test` | ❌ 已知副核初始化失败 |

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
| StarryOS vsched per-CPU 数组硬编码为 1 | ✅ 使用 `axconfig::plat::CPU_NUM` |
| `config.log=true` 仍需改 vsched2 | ✅ template 自动日志桥 |
| vDSO panic 无输出 | ✅ template panic 日志 |
| `make test` 复制整个 target 导致 ENOSPC | ✅ 只复制 release 顶层程序 |
| `Welcome to Starry OS!` | ✅ 当前自动验证通过 |
| `Hello, World!` | ✅ 旧 init 脚本已通过 |
