# Trampoline

设计理念：任何涉及到控制流的切换都经这个统一的入口进入，经过不同的处理进入到对应的目标控制流。

这里的控制流切换包括以下几类情况：

1. [x] Trap
2. [x] 任务切换

关于 Trap 相关的概念以及描述，[fast-trap](https://github.com/YdrMaster/fast-trap/blob/main/README.md) 已经描述的非常清晰，这里不进行重复。但 fast-trap 的关注点在于陷入时栈的处理以及如何快速处理 Trap。我们参考了 fast-trap 中关于栈的处理，并且基于协程实现了栈的复用。但我们的实现缺少快速处理 Trap 的支持。

根据 fast-trap 中的描述，在快速路径上，只能使用一部分的寄存器，因此只能处理少部分的 Trap，例如 gittid 等系统调用，但对于时钟中断、yield 系统调用以及其他的可能阻塞的 I/O 相关的系统调用，这仍然需要保存全套通用寄存器，进入完成的处理路径，并且执行任务切换，因此，fast-trap 的快速路径实现带来的好处与在内核中使用协程带来的好处是无法结合到一起的。在这种权衡之间，我们放弃了快速路径的支持，企图使用异步（并行）来进行加速系统调用（详细的描述见 [async_syscall](./async_syscall.md)）。

尽管牺牲了快速路径，但我们补充了 fast-trap 中关于协程切换以及栈复用的描述以及实现。（在 fast-trap 的描述文档中，它对于协程切换以及栈复用的描述仅仅是一笔带过。）

我们提供的实现建立在 fast-trap 描述的完整路径上，内核中的 Trap 处理使用单独的协程，当进入到完整路径上时，将执行这个协程进行处理，当这个协程执行系统调用让权或者时钟中断导致的抢占时，这个协程都将返回 Poll::Pending 让权，使得其他的内核协程可以继续执行（详细的描述见 [trap](./trap.md)）。

综合分析，我们的实现与 fast-trap 的设计侧重点不同，fast-trap 的设计侧重于汇编代码中的处理，如何实现快速路径；而我们的侧重点在完整路径上的 Rust 语言实现的函数逻辑，具体关注点为 Trap 的处理与内核中的协程切换如何进行结合。

## 其他的文档

- [task_ref_count](./task_ref_count.md) 中描述了如何维护任务引用计数。
- [interfaces](./interfaces.md) 中简述了对于模块化的思考，主要是关于 Features 的描述。

## 参考

- [fast-trap](https://github.com/YdrMaster/fast-trap/blob/main/README.md)