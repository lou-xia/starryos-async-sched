# Trap

## Trap 分类

按照特权级从低到高进行分类，目前仅支持了两种情况，但足以涵盖所有的情况

1. [ ] User -> User
2. [x] User -> Supervisor
3. [x] Supervisor -> Supervisor
4. [ ] Supervisor -> Hypervisor
5. [ ] Hypervisor -> Hypervisor
6. [ ] Hypervisor -> Machine
可能存在从低特权级跨越多个特权级：例如从 user -> hypervisor


关于控制流的描述，[fast-trap](https://github.com/YdrMaster/fast-trap/blob/main/README.md) 中的描述已经非常请清晰明了，这里不做过多陈述。在这里，我们将发生 Trap 时刻的全套通用寄存器称为 **Trap 上下文**。由于 Trap 时，只存在一个突发寄存器（scratch）可以使用，因此设计主要围绕着 scratch 寄存器展开。

## User -> Supervisor

目前的大多数内核仍然是使用线程来作为基础的抽象，而很少有使用协程的方式。尽管最近两年的操作系统大赛出现了使用协程写内核的队伍，但做法与线程无异（将会在后续进行描述）。这里以 [Phoenix](https://gitlab.eduxiji.net/educg-group-26010-2376550/T202418123993075-2940) 的实现为例。

```rust
pub async fn task_loop(task: Arc<Task>) {
    *task.waker() = Some(get_waker().await);
    loop {
        match task.state() {
            Terminated => break,
            Stopped => suspend_now().await,
            _ => {}
        }
        trap::user_trap::trap_return(&task);
        // task may be set to terminated by other task, e.g. execve will 
        // kill other tasks in the same thread group
        match task.state() {
            Terminated => break,
            Stopped => suspend_now().await,
            _ => {}
        }
        let intr = trap::user_trap::trap_handler(&task).await;
        match task.state() {
            Terminated => break,
            Stopped => suspend_now().await,
            _ => {}
        }
        do_signal(&task, intr).expect("do signal error");
    }
    task.do_exit();
}
```

它这里值得肯定的是将用户态任务在内核中处理（utrap_handler）、信号处理、以及进出用户态等过程放到了一个协程中，能够很好的体现出用户态任务的生命周期（详细的描述见 PLNTRY 的[技术文档](https://gitlab.eduxiji.net/PLNTRY/OSKernel2023-umi/-/blob/deployed/%E8%AE%BE%E8%AE%A1%E6%96%87%E6%A1%A3.pdf)），并且将这些过程封装成协程，能够使得多个用户态任务能够复用同一个内核栈，这种做法提供了一种新的实现方式。但我们认为它还存在改进的空间。因为它在进出用户态与内核态时，需要保存的上下文还可以简化。（它在用户态的上下文即为 fast-trap 中描述的**根控制流**，但它发生 Trap 进入到内核态的**非根控制流**则是存在状态的，非空白的。更直白的描述则是，当它通过 trap_return 回到用户态时，需要保存内核执行流（非根控制流）保证下一次进入内核态时能够执行 trap_return 的下一行代码。它在这里将 Trap 切换与任务切换（线程切换）集成到了同一个汇编函数中。）它的内核态/用户态切换是通过 trap_return 函数实现，TrapContext 是其中的关键数据，其结构如下：

```rust
pub struct TrapContext {
    // NOTE:  User to kernel should save these:
    /// General regs from x0 to x31.
    pub user_x: [usize; 32],
    /// CSR sstatus
    pub sstatus: Sstatus, // 32
    /// CSR sepc
    pub sepc: usize, // 33
    // NOTE: Kernel to user should save these:
    pub kernel_sp: usize, // 34
    pub kernel_ra: usize, // 35
    pub kernel_s: [usize; 12], // 36 - 47
    pub kernel_fp: usize, // 48
    pub kernel_tp: usize, // 49
    /// Float regs
    pub user_fx: UserFloatContext,
    /// used for the `SA_RESTART` flag of signal 
    pub last_a0: usize,
}
```

通过在 TrapContext 中记录 kernel_ra、kernel_sp、kernel_s、kernel_fp、kernel_tp 寄存器，并且在 scratch 寄存器中记录 TrapContext 的指针，从而保证在发生 Trap 时，可以保存 Trap 上下文，并且恢复内核执行流的寄存器上下文。

而 rCore-tutorial 以及一些其他的内核，则是为每个用户态任务单独准备了一个内核栈，在进入到内核态时，内核栈上为空，在内核的执行流将从初始的状态开始，因此内核执行流是不必恢复的。这些内核在 scratch 寄存器中保存了内核栈的栈顶，并将 Trap 上下文保存在内核栈上，在进出内核态时，只需要保存核恢复用户态 Trap 上下文即可。

||Phoenix|rCore-tutorial|
|-|------|--------------|
|内核栈| × | √      |
|上下文| Trap 上下文 + 内核执行流上下文 | Trap 上下文 |
|内核任务切换| 协程切换 | 线程切换 |
|任务生命周期表达|简介明了|隐晦|

综上所述，这两类的实现方式存在各自的优缺点（见上表），Phoenix 每次进出内核态需要保存核恢复的上下文开销更大（增加的开销等于一次任务切换的开销），但他实现了内核栈复用，并且使得内核的任务切换转化成为协程切换；而 rCore-tutorial 虽然减少了进出用户态需要保存/恢复上下文的开销，但必须准备单独的内核栈且内核的任务切换为线程切换。但这两种方式的实现，我们认为这两种方式是在时间和空间之间做权衡。==在系统调用较频繁，但系统调用很少阻塞或者需要让权的场景下（计算密集型），Phoenix 内核中使用协程切换减少的任务切换开销相较于系统调用增加的开销显得微不足道==；==在执行阻塞系统调用较频繁的场景下（I/O密集型），rCore-tutorial 由于阻塞导致的任务切换的开销则会显著增加==。这些分析印证了线程适用于计算密集型，而协程适用于 I/O 密集型的场景。

因此，我们的设计宗旨是尽可能的结合两种模式的优点。

1. 使用协程写内核；
2. 减少任务切换开销；
3. 减少进出内核态开销；
4. 内核栈复用；

鉴于第 1 条宗旨，Phoenix 的做法是值得我们借鉴的，我们在此基础上，经过仔细分析，认为它增加的进出内核态的开销是由于没有对用户态任务在内核中的行为进行正确的分析，它错误的将 trap_return 以及 trap 也放到了协程中。我们认为 trap_return 以及 trap 这两个过程不属于的内核协程，因为他们的执行编译器不会保证生成状态机，需要从这个协程中剥离，仅仅保留 utrap_handler 以及信号处理的部分。用户态任务在内核中仅仅对应于一个 **utrap_handler 协程**。当用户态执行流发生 trap 时，只需要将 Trap 上下文保存起来，并使用一个新的栈来执行 **trap_handler** 协程即可（这个执行流不能保存在新的栈上，否则即会导致不能栈复用）。再根据这个 utrap_handler 协程的处理情况，从而确定是否需要进行任务切换或者返回用户态。由于在 Trap 时，是从一个新的栈上开始执行，进出内核态需要保存和恢复的开销减小了，并且由于任务切换为协程切换，导致任务切换的开销减小了，也达到了栈复用的目标。

我们的具体做法是在 scratch 寄存器中记录 TrapFrame 的指针，从而在发生 Trap 时，能够正确的保存上下文，并在其中记录内核栈（可以复用）的栈顶。但由于内核的执行流没有在栈上保存，因此对于阻塞系统调用以及一些其他的可能无法及时处理的情况，我们需要在 TrapFrame（见下） 中记录 scause 以及 stval 等寄存器信息，以保证在进行了任务切换之后，下一次还能进行正确的处理。

```rust
/// Saved registers when a trap (interrupt or exception) occurs.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct TrapFrame {
    /// All general registers.
    pub regs: GeneralRegisters,
    /// Supervisor Exception Program Counter.
    pub sepc: usize,
    /// Supervisor Status Register.
    pub sstatus: usize,
    /// 浮点数寄存器
    pub fs: [usize; 2],
    /// Supervisor Cause Register
    pub scause: usize,
    /// Supervisor Trap Value
    pub stval: usize,
    /// 表示是否需要进行处理
    pub trap_status: TrapStatus,
    /// 进入用户态时，记录的内核栈指针
    pub kernel_sp: usize,
}
```

具体的代码逻辑如下，需要手动组合 utrap_handler 协程以及 trap_return、trap 的逻辑（因为 Rust 协程不支持额外的 Pending 状态返回，因此这里增加了额外的状态字段，来判断 utrap_handler 协程让权后的处理逻辑）。

```rust
pub fn run_task(task: &TaskRef) {
    loop {
        // 运行 utrap_handler 协程
        let res = task.get_fut().as_mut().poll(cx);
        match res {
            Poll::Ready(exit_code) => {
                // 协程结束，意味着对应的用户态任务的生命周期结束，需要退出
                task.exit();
            },
            Poll::Pending => {
                // 协程让权，系统调用处理完毕需要回到用户态
                if let Some(tf) = task.utrap_frame() {
                    if tf.trap_status == TrapStatus::Done {
                        unsafe { tf.user_return(); }
                    }
                }
                // 由于系统调用阻塞，utrap_handler 协程让权
                CurrentTask::clean_current_without_drop();
            }
        }
    }
}
```

在上述的代码中，loop 循环是可有可无的，增加只是为了能够使得对用户态任务的生命周期描述更加清晰。由于 trap_return 是在 loop 循环的结尾，在进入用户态后，内核的执行流是不必保存的。因此，在实际的过程中，每次进入到 trampoline 时，都会由汇编代码进入到 run_task 的函数，从这个函数重新开始执行 utrap_handler 协程，从而形成用户态任务生命周期的循环，可以去掉 loop。

## Supervisor -> Supervisor

对于在内核态发生 Trap 的情况，也可以参考上述 User -> Supervisor 的设计，使用 scratch 寄存器记录 TrapFrame 的指针，从而在发生 Trap 时可以保证上下文保存，并且在新的栈上执行内核的 strap_handler 协程进行处理。这种方式可以很好的应对单个 Trap 的情况，但难以应对 Trap 嵌套的情形，当用户态任务由于系统调用进入内核态执行 utrap_handler 协程时，此时发生了时钟中断，产生了 Trap 嵌套，这意味着一个任务控制块需要两个 TrapFrame 的空间来分别记录上下文，并且在从 User -> Supervisor 的时，完成 scratch 寄存器的切换，尽管这种方式可以实现，但增加了代码的逻辑。并且经过分析，在进入内核态后，需要使用内核栈。在不支持抢占的情况下，内核发生 Trap 处理完成后，执行流会原路返回，这种情况下，将 TrapFrame 直接保存在内核栈是最直接的方式；在需要抢占时，即使使用了单独的空间保存 TrapFrame，但内核执行 utrap_handler 协程的函数调用关系仍然是保存在内核栈上的，这个内核栈是与 utrap_handler 协程绑定的，抢占也需要使用一个新栈才可以运行其他的任务，因此直接将 TrapFrame 保存在内核栈上是合适的。因此，在 User -> Supervisor 时，scratch 寄存器将被清空，此时若再发生 Trap，可以根据 scratch 寄存器区分发生 Trap 时的特权级。

## 参考

- [fast-trap](https://github.com/YdrMaster/fast-trap/blob/main/README.md)
- [Phoenix](https://gitlab.eduxiji.net/educg-group-26010-2376550/T202418123993075-2940)
- [PLNTRY](https://gitlab.eduxiji.net/PLNTRY/OSKernel2023-umi)