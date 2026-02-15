# async_syscall

关于系统调用的优化，存在两种方向：

1. 减少由于系统调用导致的特权级以及上下文切换开销
2. 异步批处理

其中方向 1 由多种实现方式，例如 [UserBypass](https://www.usenix.org/conference/osdi23/presentation/zhou-zhe)，将连续系统调用直接在内核中进行处理，避免了来回进出用户态；而 [fast-trap](https://github.com/YdrMaster/fast-trap/blob/main/README.md) 的优化则是减少了执行部分系统调用时需要保存的寄存器数量，优化效果有限。还存在其他的很多工作围绕着减少特权级、上下文保存恢复开销展开。

而方向 2 的代表作有 [FlexSC](https://www.usenix.org/conference/osdi10/flexsc-flexible-system-call-scheduling-exception-less-system-calls)，它将系统调用进行了批处理，利用了多核并行来进行加速。而我们的实现则是基于方向 2。

我们将用户态任务在内核中的 Trap 处理过程使用一个 utrap_handler 协程来实现，将 trap_return、trap 的过程与这个 utrap_handler 协程分离（详细的描述见 [trap](./trap.md)），从而使得用户态任务在用户态的执行流与对应的 utrap_handler 可以运行在不同的核上，从而利用多处理器实现并行异步批处理加速。

具体的实现如下，当需要开启加速时，先以同步的方式进入到内核态，在 utrap_handler 协程的处理过程中，设置任务控制块中的相关字段，建立共享内存用于传递系统调用信息，并将这个 utrap_handler 放到其他核的就绪队列上（是否抢占可根据优先级进行灵活处理）。

## 参考

- [UserBypass](https://www.usenix.org/conference/osdi23/presentation/zhou-zhe)
- [FlexSC](https://www.usenix.org/conference/osdi10/flexsc-flexible-system-call-scheduling-exception-less-system-calls)
- [fast-trap](https://github.com/YdrMaster/fast-trap/blob/main/README.md)