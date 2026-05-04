//! vsched2 接口适配层：按照 vDSO 模式，通过 `libvsched2` 包装库将
//! StarryOS 的任务/调度/上下文等子系统接入 vsched2 统一调度框架。
//!
//! # 背景
//!
//! vsched2 的职责是提供一套"统一调度框架"，不由外部创建任务或管理内核对象。
//! 外部系统需要负责：定义真实任务对象、保存/恢复任务上下文、提供栈分配与回收、
//! 提供地址空间切换、提供 trap 处理任务、在合适的时机把控制权交给调度器。
//!
//! vsched2 内部则负责：维护调度循环、维护就绪队列与事件源、在内核态/用户态
//! 之间切换调度逻辑、基于优先级在多个进程和事件源之间选择下一任务。
//!
//! vsched2 的接口分为两类：
//! 1. **外部需要提供的接口**（本节）—— 以 trait_interface! 声明的适配接口，
//!    必须由外部系统实现（对应 vsched2/src/interface.rs）
//! 2. **vsched2 对外提供的接口**（不由本节实现）—— 调度入口 `raw_trap_entry`、
//!    `raw_thread_entry` 等，通过汇编符号暴露给内核调用
//!
//! # 设计思路
//!
//! 本模块遵循 `vdso/src/lib.rs` 中展示的 vDSO 模式：
//! 1. 为 vsched2 的每个 `trait_interface!` 接口实现一个 StarryOS 适配器
//! 2. 调用 `libvsched2::init_vtable_<Trait>::<Adapter>()` 注册到 vsched2 虚表
//! 3. 所有接口使用类型安全的 trait impl（编译期验证），而非裸函数指针
//!
//! vsched2 以 `*const ()` 裸指针持有任务对象（见接口文档 §8.2 指针语义）。
//! 这种"类型擦除"意味着 vsched2 不拥有任务对象的具体类型定义，
//! 外部实现必须保证任务对象的生命周期和地址有效性。
//! 本模块中，StarTask 会被 `Box::leak` 转为 `&'static` 生命周期后传裸指针给 vsched2。
//!
//! # 关于 AxTask 作为调度对象
//!
//! 设计上选择直接包装 `AxTaskRef`（即 `Arc<AxTask>`）而不使用外部分离的
//! task_metadata 映射表。原因如下：
//! - AxTaskRef 本身就是 StarryOS 的任务句柄，包含了任务 ID、状态、栈等核心信息
//! - vsched2 Task trait 的方法签名为 `&self`，恰好匹配 AxTaskRef 的引用语义
//! - 内嵌原子字段（priority, pid 等）避免了额外的 HashMap 查找开销
//! - 创建时 Box::leak 转为裸指针，vsched2 通过 virt impl 机制还原为 trait 引用，
//!   整个过程中不需要外部映射表来查找 task_metadata

use alloc::{
    alloc::{Layout, alloc, dealloc},
    boxed::Box,
    sync::Arc,
};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering},
    task::Poll,
};

use axhal::{mem::phys_to_virt, percpu::this_cpu_id};
use axmm::{AddrSpace, kernel_aspace};
use axsync::Mutex;
use axtask::{AxTaskRef, TaskState as AxTaskState};
use lazy_static::lazy_static;
use memory_addr::{PhysAddr, VirtAddr};

// libvsched2 重新导出了 vsched2 的定义（trait、类型等）以及 vtable 初始化函数。
// 包括 Task, Stack, Context, TrapHandle, SMP, VSpace, UserData trait 和
// init_vtable_Task, init_vtable_Stack 等泛型初始化函数。
use libvsched2;

use crate::{
    config,
    task::AsThread,
};

// ============================================================================
// 常量定义
// ============================================================================

/// vsched2 调度优先级的上界（最高优先级），数值越小优先级越高。
/// 对应 vsched2 build.rs 中的 `HIGHEST_PRIORITY` 配置。
#[allow(dead_code)]
const HIGHEST_PRIORITY: isize = 0;

/// vsched2 调度优先级的下界（最低优先级），数值越小优先级越高。
/// 对应 vsched2 build.rs 中的 `LOWEST_PRIORITY` 配置。
#[allow(dead_code)]
const LOWEST_PRIORITY: isize = 15;

// ============================================================================
// 1. TaskState — StarryOS 与 vsched2 的状态映射
// ============================================================================

/// 将 axtask::TaskState 映射为 vsched2::TaskState。
///
/// StarryOS（axtask）：Running=1, Ready=2, Blocked=3, Exited=4
/// vsched2：Ready=0, Running=1, Blocked=2, Exited=3
fn to_vsched_state(s: AxTaskState) -> libvsched2::TaskState {
    match s {
        AxTaskState::Ready => libvsched2::TaskState::Ready,
        AxTaskState::Running => libvsched2::TaskState::Running,
        AxTaskState::Blocked => libvsched2::TaskState::Blocked,
        AxTaskState::Exited => libvsched2::TaskState::Exited,
    }
}

fn from_vsched_state(s: libvsched2::TaskState) -> AxTaskState {
    match s {
        libvsched2::TaskState::Ready => AxTaskState::Ready,
        libvsched2::TaskState::Running => AxTaskState::Running,
        libvsched2::TaskState::Blocked => AxTaskState::Blocked,
        libvsched2::TaskState::Exited => AxTaskState::Exited,
    }
}

// ============================================================================
// 2. StarTask — vsched2 Task trait 的 StarryOS 实现
// ============================================================================

/// 协程轮询接口。
///
/// vsched2 支持两类任务调度：线程式（直接恢复寄存器上下文）和协程式（调用 poll）。
/// 实现该 trait 的对象可以被 StarTask 包装为协程任务：
/// - `poll()` 返回 `Poll::Ready(value)` → 协程执行完毕，value 为返回值
/// - `poll()` 返回 `Poll::Pending` → 协程主动让权，后续可再次调度
pub trait CoroutinePoll: Send + Sync {
    /// 轮询协程一次。
    fn poll(&self) -> Poll<usize>;
}

/// StarTask：将 StarryOS 的 `AxTaskRef` 包装为 vsched2 `Task` trait 的实现。
///
/// # 设计说明
///
/// - **任务对象复用**：直接包装 `AxTaskRef` 而非维护外部映射表（task_metadata），
///   避免了 HashMap 查找开销和映射表生命周期管理问题。
///
/// - **额外元数据内嵌**：vsched2 需要的 priority、pid、is_coroutine 等字段
///   以 `Atomic*` 类型内嵌在 StarTask 中，线程安全读写。
///
/// - **指针生命周期**：创建时通过 `Box::leak` 转为 `&'static Self`，再获取
///   裸指针传递给 vsched2。vsched2 以 `*const ()` 持有（类型擦除，见接口文档 §8.2），
///   通过 `TaskVirtImpl::from_ptr` 还原为 trait 引用调用。
///   外部实现必须保证该指针在 vsched2 整个使用周期内有效。
///
/// - **优先级约束**：priority 值应符合 vsched2 的 `0..=15` 范围（默认配置），
///   数值越小优先级越高。见接口文档 §3.1 priority 说明。
pub struct StarTask {
    /// 底层 StarryOS 任务引用
    task: AxTaskRef,
    /// 任务优先级（数值越小优先级越高，建议范围 0..=15）
    priority: AtomicIsize,
    /// 所属进程号（全局进程表 PROCESS_INFO_TABLE 的索引，0 保留给内核）
    pid: AtomicUsize,
    /// 是否为协程任务（true: 协程，false: 线程）
    is_coroutine: AtomicBool,
    /// 协程执行的返回值（在 poll() 返回 Ready(value) 时写入）
    return_value: AtomicUsize,
    /// 线程上下文对应的栈底地址
    thread_stack_base: usize,
    /// 协程轮询实现（None 表示线程任务）
    coroutine: Option<Arc<dyn CoroutinePoll>>,
}

impl StarTask {
    /// 创建一个新的 StarTask 适配器。
    ///
    /// # 参数
    ///
    /// - `task`：底层 StarryOS 任务引用
    /// - `priority`：任务优先级，应符合 `HIGHEST_PRIORITY..=LOWEST_PRIORITY` (0..=15)
    /// - `pid`：所属进程号（0 表示内核态任务）
    /// - `coroutine`：协程轮询实现，`None` 表示线程任务
    pub fn new(
        task: AxTaskRef,
        priority: isize,
        pid: usize,
        coroutine: Option<Arc<dyn CoroutinePoll>>,
    ) -> Self {
        // 从 AxTaskRef 获取内核栈底地址：栈顶地址减去栈大小即为栈底
        let stack_base = task
            .kernel_stack_top()
            .map(|top| top.as_usize().saturating_sub(config::KERNEL_STACK_SIZE))
            .unwrap_or(0);
        Self {
            task,
            priority: AtomicIsize::new(priority),
            pid: AtomicUsize::new(pid),
            is_coroutine: AtomicBool::new(coroutine.is_some()),
            return_value: AtomicUsize::new(0),
            thread_stack_base: stack_base,
            coroutine,
        }
    }

    /// 返回底层 StarryOS 任务引用。
    pub fn inner(&self) -> &AxTaskRef {
        &self.task
    }
}

impl libvsched2::Task for StarTask {
    /// 获取当前任务状态。
    ///
    /// 返回值说明（vsched2 `TaskState`）：
    /// - `Ready`（0）：可运行，等待调度
    /// - `Running`（1）：正在执行
    /// - `Blocked`（2）：已阻塞，暂不可运行
    /// - `Exited`（3）：已退出
    ///
    /// 调度器在回收当前任务、重新入队或判断任务生命周期时调用。
    fn state(&self) -> libvsched2::TaskState {
        to_vsched_state(self.task.state())
    }

    /// 设置任务状态，并返回**设置前的旧状态**。
    ///
    /// 按照 vsched2 接口文档的建议（§3.1 set_state），
    /// 返回值应为被覆盖之前的状态，便于状态转换检查。
    /// vsched2 通过此接口将任务设为 Ready/Running/Blocked/Exited。
    fn set_state(&self, state: libvsched2::TaskState) -> libvsched2::TaskState {
        let old = to_vsched_state(self.task.state());
        self.task.set_state(from_vsched_state(state));
        old
    }

    /// 获取任务优先级。
    ///
    /// 返回值越小表示优先级越高。取值范围应与编译期配置
    /// `HIGHEST_PRIORITY..=LOWEST_PRIORITY` 一致（默认 0..=15）。
    fn priority(&self) -> isize {
        self.priority.load(Ordering::Acquire)
    }

    /// 判断任务是协程还是线程。
    ///
    /// - `true`：协程任务，调度器会调用 `poll()` 而非恢复寄存器上下文
    /// - `false`：线程任务，调度器会调用 `restore_context()` 恢复寄存器上下文
    fn is_coroutine(&self) -> bool {
        self.is_coroutine.load(Ordering::Acquire)
    }

    /// 获取任务所属地址空间对应的进程号。
    ///
    /// 返回全局进程表 `PROCESS_INFO_TABLE` 中的索引。
    /// 若返回 0，表示该任务尚未设置 pid（或为内核态任务）。
    /// 此值用于地址空间切换；某些内核任务也可能属于某个用户进程地址空间。
    fn pid(&self) -> usize {
        self.pid.load(Ordering::Acquire)
    }

    /// 设置任务所属地址空间的进程号。
    ///
    /// 主要用于 trap 处理任务继承被打断任务所属的地址空间。
    /// 进程号即为全局进程表 `PROCESS_INFO_TABLE` 中的索引。
    fn set_pid(&self, pid: usize) {
        self.pid.store(pid, Ordering::Release);
    }

    /// 保存线程上下文。
    ///
    /// 当协程主动让权后，vsched2 调用此方法保存当前线程上下文，
    /// 以便后续返回调度循环。此处将任务状态标记为 Ready 表示可重新调度。
    fn save_thread_context(&self) {
        self.task.set_state(AxTaskState::Ready);
    }

    /// 保存 trap 上下文。
    ///
    /// 当前 vsched2 调度循环主体尚未直接调用此接口（见接口文档 §3.1），
    /// 但它是外部任务上下文管理的一部分，预留给 trap 场景使用。
    /// 此处将任务状态标记为 Blocked 以进入等待处理的状态。
    fn save_trap_context(&self) {
        self.task.set_state(AxTaskState::Blocked);
    }

    /// 恢复任务寄存器上下文。
    ///
    /// 当线程任务被调度运行时，vsched2 调用此方法恢复寄存器上下文。
    /// **正常情况下不应返回到调用点**（函数内部应直接完成上下文切换）。
    /// 目前尚未接入 vsched2 的实际上下文切换路径（panic 占位）。
    fn restore_context(&self) {
        panic!(
            "StarTask::restore_context: vsched2 context switching not yet integrated. \
             task={}",
            self.task.id_name()
        );
    }

    /// 运行协程任务一次。
    ///
    /// vsched2 在协程调度路径上调用：
    /// - `Poll::Ready(value)`：协程已执行完毕，value 为返回值。
    ///   vsched2 随后会调用 `set_return_value(value)` 记录。
    /// - `Poll::Pending`：协程主动让权，后续可再次被调度。
    ///
    /// 对于非协程任务（coroutine 为 None），直接返回 Poll::Ready。
    fn poll(&self) -> Poll<usize> {
        match self.coroutine.as_ref() {
            Some(coro) => {
                let polled = coro.poll();
                if let Poll::Ready(value) = polled {
                    self.return_value.store(value, Ordering::Release);
                }
                polled
            }
            None => Poll::Ready(self.return_value.load(Ordering::Acquire)),
        }
    }

    /// 获取线程上下文对应的栈底地址。
    ///
    /// vsched2 在切换栈时使用此地址来判断是否需要切换或回收栈。
    /// 对于 RISC-V 64 位：栈帧范围为 `(fp, sp]`，
    /// ra 和 fp 的先前值分别存放在 `fp-8` 和 `fp-16` 处。
    fn thread_stack_base(&self) -> usize {
        self.thread_stack_base
    }

    /// 写入协程任务的返回值。
    ///
    /// vsched2 在 `poll()` 返回 `Poll::Ready(value)` 后调用，
    /// 将 value 记录到任务对象中。
    fn set_return_value(&self, value: usize) {
        self.return_value.store(value, Ordering::Release);
    }
}

/// 将 vsched2 返回的裸指针还原为 StarryOS 的 `AxTaskRef`。
///
/// # Safety
///
/// 调用方必须保证 `task` 是由本模块的 `register_task` 创建的 StarTask 裸指针，
/// 且指向的内存在调用时仍然有效（StarTask 为 Box::leak 的静态生命周期，所以
/// 在内核运行期间始终有效）。
///
/// vsched2 以 `*const ()` 类型擦除后的指针传递任务对象（见接口文档 §8.2）。
/// 本函数将其还原为有类型的 StarTask 引用，并取出内部的 AxTaskRef。
pub fn task_from_raw(task: *const ()) -> Option<AxTaskRef> {
    if task.is_null() {
        return None;
    }
    // Safety: see above
    let star = unsafe { &*(task as *const StarTask) };
    Some(star.task.clone())
}

/// 注册一个新任务到 vsched2 调度框架，并返回其裸指针供 vsched2 持有。
///
/// 此函数创建一个 StarTask 适配器，通过 `Box::leak` 将其转为堆分配的静态引用，
/// 然后获取裸指针传递给 vsched2。vsched2 在后续调度中以 `*const ()` 持有此指针。
///
/// # 参数
///
/// - `task`：底层的 StarryOS 任务引用（AxTaskRef = Arc<AxTask>）
/// - `priority`：任务优先级，值越小越高（建议 0..=15）
/// - `pid`：所属进程号，0 表示内核任务
/// - `coroutine`：协程轮询实现，None 表示普通线程任务
///
/// # 生命周期
///
/// 返回的裸指针在 Box::leak 后具备 `'static` 生命周期，
/// vsched2 可安全地在任意时刻解引用该指针。
/// 外部实现必须保证这些任务在 vsched2 的整个使用周期内不被提前释放。
pub fn register_task(
    task: AxTaskRef,
    priority: isize,
    pid: usize,
    coroutine: Option<Arc<dyn CoroutinePoll>>,
) -> *const StarTask {
    let star = Box::new(StarTask::new(task, priority, pid, coroutine));
    Box::into_raw(star)
}

// ============================================================================
// 3. Stack — 调度器栈分配/回收
// ============================================================================

/// vsched2 Stack trait 的 StarryOS 实现。
///
/// 按照接口文档要求（§3.2）：
/// - `alloc()` 返回的栈必须可用于后续上下文切换，且满足目标架构的对齐要求
///   （此处按 16 字节对齐）
/// - `dealloc()` 调用时外部应确保栈不再被任何执行流使用
///
/// 当前实现直接使用全局分配器（alloc crate），分配 `KERNEL_STACK_SIZE` 大小的栈。
/// 之后可替换为专用的栈池管理器（参考 vsched2 内部的 StackHandler/KERNEL_STACKS）。
struct StarryStack;

impl libvsched2::Stack for StarryStack {
    fn alloc() -> *mut () {
        let layout = Layout::from_size_align(config::KERNEL_STACK_SIZE, 16)
            .expect("StarryStack: invalid scheduler stack layout");
        let ptr = unsafe { alloc(layout) };
        NonNull::new(ptr)
            .expect("StarryStack: failed to allocate scheduler stack")
            .cast()
            .as_ptr()
    }

    fn dealloc(stack: *mut ()) {
        let layout = Layout::from_size_align(config::KERNEL_STACK_SIZE, 16)
            .expect("StarryStack: invalid scheduler stack layout");
        unsafe { dealloc(stack.cast(), layout) };
    }
}

// ============================================================================
// 4. Context — 特权级切换和地址空间切换
// ============================================================================

/// vsched2 Context trait 的 StarryOS 实现。
///
/// 封装内核态/用户态切换以及地址空间相关的底层行为。
/// 按照接口文档（§3.3）：
///
/// - **`into_kernel()`**：从用户态调度器主动陷入内核。当前尚未接入实际路径。
/// - **`into_user()`**：从内核态调度到用户协程时调用，参数为用户栈顶（sp 值）。
///   当前尚未接入实际路径。
/// - **`into_user_context()`**：从内核态调度到用户线程时调用，参数为任务指针。
///   当前尚未接入实际路径。
/// - **`switch_vspace()`**：切换地址空间。根据接口文档 §3.6 说明，
///   真正被调度循环调用的主要是 `VSpace::into_vspace`，
///   `Context::switch_vspace` 目前更像预留接口。
///
/// 注意：当前接口层未强制地址空间对象的具体类型（§3.3 switch_vspace 说明），
/// 调度模块只要求该指针能被外部实现识别。此处我们约定该指针指向 `AddrSpace`。
struct StarryContext;

impl libvsched2::Context for StarryContext {
    fn into_kernel() -> ! {
        panic!("StarryContext::into_kernel: vsched2 trap entry not yet integrated");
    }

    fn into_user(_ustack: usize) {
        panic!("StarryContext::into_user: vsched2 user trampoline not yet integrated");
    }

    fn into_user_context(_task: *const ()) {
        panic!(
            "StarryContext::into_user_context: vsched2 user trampoline not yet integrated"
        );
    }

    /// 切换当前 CPU 的地址空间。
    ///
    /// 根据接口文档（§3.3 switch_vspace 和 §8.3 地址空间切换约束）：
    /// - 参数指向外部定义的地址空间对象；此处约定为指向 `AddrSpace` 的指针
    /// - 切换地址空间后，不应继续使用之前从 `UserData::get_user_data()` 获得的引用
    /// - 若参数为 null 或页表为空则静默返回
    fn switch_vspace(vspace_pid: *const ()) {
        if vspace_pid.is_null() {
            return;
        }
        let aspace = unsafe { &*(vspace_pid as *const AddrSpace) };
        let root = aspace.page_table_root();
        if root.as_usize() == 0 {
            return;
        }
        let current_root = axhal::asm::read_user_page_table();
        if current_root != root {
            unsafe { axhal::asm::write_user_page_table(root) };
            // 注：切换地址空间后需要刷新 TLB（sfence.vma），
            // 目前依赖硬件/后续页表遍历时自动处理。
        }
    }
}

// ============================================================================
// 5. TrapHandle — Trap 处理任务工厂
// ============================================================================

// lazy_static! 不支持 /// 文档注释（rustdoc 不解析 macro_rules 宏的 doc），
// 因此以下使用 // 注释。
//
// 全局 trap 处理器工厂。
// 通过 `register_trap_handler_factory` 注册后，vsched2 在发生同步 trap 时
// 通过 `TrapHandle::get_handler` 获取处理该 trap 的内核任务。
//
// 按照接口文档（§3.4）的期望行为：
// - 优先从阻塞队列中取出已有 trap 处理任务
// - 若数量不足，可创建新的处理任务
// - 处理任务应能接收原任务并分析 trap 原因
lazy_static! {
    static ref TRAP_HANDLER_FACTORY: Mutex<
        Option<Arc<dyn Fn(*const ()) -> *const () + Send + Sync>>,
    > = Mutex::new(None);
}

/// 注册一个 trap 处理任务的工厂函数。
///
/// 根据接口文档（§3.4 TrapHandle），当 vsched2 中发生同步 trap 时，
/// `raw_trap_entry` → `trap_entry` → `trap_handle()` 调用链会通过
/// `TrapHandle::get_handler()` 获取处理该 trap 的内核任务。
///
/// factory 接收被 trap 的原任务裸指针，应返回一个可运行的 trap 处理任务裸指针。
/// 返回的指针不应为空（vsched2 期望一个有效的 Task 对象）。
///
/// 实现约定：
/// - trap 处理任务应继承原任务的 pid，以保证可访问原任务所在地址空间（§8.3）
/// - 工厂函数应支持并发调用（不同 CPU 可能同时触发 trap）
pub fn register_trap_handler_factory(
    factory: Arc<dyn Fn(*const ()) -> *const () + Send + Sync>,
) {
    *TRAP_HANDLER_FACTORY.lock() = Some(factory);
}

/// vsched2 TrapHandle trait 的 StarryOS 实现。
///
/// 根据接口文档（§3.4），该接口的职责是为被 trap 的任务获取或创建一个
/// 专门的 trap 处理任务。本实现通过全局工厂函数完成。
struct StarryTrapHandle;

impl libvsched2::TrapHandle for StarryTrapHandle {
    /// 根据被 trap 的任务获取一个 trap 处理任务。
    ///
    /// 按照接口文档（§3.4 get_handler）：
    /// - `task`：被 trap 的原任务对象指针（类型擦除，即 vsched2 内部持有的 `*const ()`）
    /// - 返回值：trap 处理任务对象指针。vsched2 将此指针传递给 `TrapHandle` 的后续调用。
    ///
    /// 如果尚未注册工厂函数，此处会触发 panic。
    /// 正常情况下工厂函数应在系统初始化时通过 `register_trap_handler_factory` 注册。
    fn get_handler(task: *const ()) -> *const () {
        let guard = TRAP_HANDLER_FACTORY.lock();
        let factory = guard
            .as_ref()
            .expect("TrapHandle factory not registered. \
                     Call register_trap_handler_factory() before vsched2 starts scheduling.");
        let ptr = factory(task);
        assert!(
            !ptr.is_null(),
            "TrapHandle factory returned null for task {:p}",
            task
        );
        ptr
    }
}

// ============================================================================
// 6. SMP — 多核支持
// ============================================================================

/// vsched2 SMP trait 的 StarryOS 实现。
///
/// 按照接口文档（§3.5），向调度器提供当前 CPU 标识。
/// 返回值应在 `0..CPU_NUM` 范围内。
///
/// 使用场景：获取当前任务、访问 per-CPU 共享变量（如 CURRENT_TASK、IN_KERNEL 等）、
/// 选择当前 CPU 对应的栈池状态。
struct StarrySmp;

impl libvsched2::SMP for StarrySmp {
    fn cpu_id() -> usize {
        this_cpu_id()
    }
}

// ============================================================================
// 7. VSpace — 地址空间切换（调度循环主路径）
// ============================================================================

/// vsched2 VSpace trait 的 StarryOS 实现。
///
/// 按照接口文档（§3.6）：
/// - 真正被 vsched2 调度循环调用的是 `VSpace::into_vspace`，
///   而非 `Context::switch_vspace`
/// - 参数来自 `ProcessInfo.vspace`，类型为 `*mut ()`（类型擦除）
/// - 本实现中约定该指针指向 `AddrSpace`
///
/// 注意（§8.3 地址空间切换约束）：
/// 切换地址空间时 `CURRENT_VSPACE` 记录的是目标地址空间所属进程号。
struct StarryVSpace;

impl libvsched2::VSpace for StarryVSpace {
    fn into_vspace(vspace: *mut ()) {
        if vspace.is_null() {
            return;
        }
        let aspace = unsafe { &*(vspace as *const AddrSpace) };
        let root = aspace.page_table_root();
        if root.as_usize() == 0 {
            return;
        }
        let current_root = axhal::asm::read_user_page_table();
        if current_root != root {
            unsafe { axhal::asm::write_user_page_table(root) };
        }
    }
}

// ============================================================================
// 8. UserData — vDSO 私有数据映射
// ============================================================================

/// 在给定的地址空间中，查找目标物理地址对应的用户态虚拟地址。
///
/// 遍历地址空间的所有映射区域，通过页表查询找到包含目标物理页的虚拟地址。
fn find_user_vaddr_for_phys(aspace: &AddrSpace, target: PhysAddr) -> Option<VirtAddr> {
    for area in aspace.areas() {
        let mut vaddr = area.start();
        while vaddr < area.end() {
            if let Ok((paddr, ..)) = aspace.page_table().query(vaddr)
                && paddr == target
            {
                return Some(vaddr);
            }
            vaddr += 4096;
        }
    }
    None
}

/// vsched2 UserData trait 的 StarryOS 实现。
///
/// 按照接口文档（§3.7），从内核态访问"当前地址空间下用户态 vDSO 私有数据区"
/// 的对应对象。将内核侧 vVAR 数据区的内核虚拟地址翻译为用户空间中的对应地址。
///
/// # 安全约束（见接口文档 §3.7）
///
/// - 返回地址必须位于用户态 vDSO 私有数据区内
/// - `[addr, addr + len)` 必须完整可访问
/// - **切换地址空间前后不能继续使用同一份映射引用**（§8.3）
///
/// 参数 `pos` 和 `len` 由 vsched2 内部传入，代表它在 vVAR 区中要访问的
/// 共享数据对象在内核地址空间中的位置和大小。
struct StarryUserData;

impl libvsched2::UserData for StarryUserData {
    fn get_user_data(pos: usize, len: usize) -> *mut () {
        // 读取 vDSO 加载时记录的 vVAR 元数据
        let vvar_start_pa = unsafe { crate::vsched::VSCHED2_VVAR_START_PA };
        let vvar_size = unsafe { crate::vsched::VSCHED2_VVAR_SIZE };

        // 安全检查：请求长度不能超过 vVAR 总大小
        if len > vvar_size {
            return core::ptr::null_mut();
        }

        // 将 vVAR 起始物理地址翻译为内核虚拟地址
        let kernel_vvar_start = phys_to_virt(PhysAddr::from(vvar_start_pa)).as_usize();
        let kernel_vvar_end = kernel_vvar_start + vvar_size;
        let end = match pos.checked_add(len) {
            Some(e) => e,
            None => return core::ptr::null_mut(),
        };

        // 安全检查：pos 必须在 vVAR 内核映射范围内
        if pos < kernel_vvar_start || end > kernel_vvar_end {
            return core::ptr::null_mut();
        }

        let offset = pos - kernel_vvar_start;

        // 获取当前任务所属进程的地址空间
        let current = axtask::current();
        let Some(thr) = current.try_as_thread() else {
            return core::ptr::null_mut();
        };

        // 通过页表查询，找到目标物理页在用户地址空间中对应的虚拟地址
        let aspace = thr.proc_data.aspace.lock();
        let target_page_pa = PhysAddr::from((vvar_start_pa + offset) & !0xfff);
        let Some(user_page) = find_user_vaddr_for_phys(&aspace, target_page_pa) else {
            return core::ptr::null_mut();
        };

        (user_page.as_usize() + offset % 4096) as *mut ()
    }
}

// ============================================================================
// 9. vDSO 全局元数据
// ============================================================================

/// vsched2 vDSO 的 vVAR 数据区起始物理地址。
///
/// 在 `init_vsched2_interfaces()` 中初始化后可供 UserData 等模块使用。
/// 通过物理地址+页表遍历，可将内核侧的 vVAR 访问翻译为用户地址空间中的对应地址。
pub static mut VSCHED2_VVAR_START_PA: usize = 0;

/// vsched2 vVAR 数据区大小（字节）。
pub static mut VSCHED2_VVAR_SIZE: usize = 0;

/// vsched2 vDSO 代码段起始物理地址。
pub static mut VSCHED2_VDSO_START_PA: usize = 0;

/// vsched2 vDSO 代码段大小（字节）。
pub static mut VSCHED2_VDSO_SIZE: usize = 0;

// ============================================================================
// 10. 初始化入口
// ============================================================================

static VSCHED2_READY: AtomicBool = AtomicBool::new(false);

/// 初始化 vsched2 接口：加载 `libvsched2.so` 并注册所有接口 trait 实现。
///
/// # 调用时机
///
/// 该函数应在内核初始化早期调用（在任务调度启动前）。
/// 当前从 `src/main.rs` 中调用。
///
/// # 初始化流程
///
/// 1. **加载 vDSO**：调用 `libvsched2::load_and_init(vspace)` 在内核地址空间中
///    映射 vsched2 的 .so 文件并初始化内核侧虚表（vdso_vtable）。
///    此过程由 `build_vdso` 生成的包装代码自动完成。
///
/// 2. **注册接口实现**：调用 7 个 `init_vtable_*::<Adapter>()`，
///    将 StarryOS 的适配器实现注册到 vsched2 的虚表中。
///    这些泛型调用在编译期即可验证 trait 实现的完整性。
///
/// 3. **记录元数据**：保存 vDSO/vVAR 的物理地址信息，供 UserData 等模块后续使用。
///
/// # TODO
///
/// - vDSO/vVAR 元数据当前使用占位值。需要从 `libvsched2` 获取实际映射信息。
/// - `load_and_init` 不返回 vDSO 映射地址，需要扩展生成的 API 或直接调用 `map_so`。
pub fn init_vsched2_interfaces() {
    if VSCHED2_READY.swap(true, Ordering::AcqRel) {
        return; // 已初始化，幂等
    }

    // 步骤 1: 在内核地址空间中加载 vsched2 vDSO 并初始化内核侧 vtable。
    // load_and_init 内部调用 map_so（映射 .so 到虚拟地址空间）和
    // init_vdso_vtable（从 .dynsym 解析 init_vtable_* 函数地址并填充 VDSO_VTABLE）。
    #[allow(unused_variables)]
    let vdso_start = {
        let mut aspace = kernel_aspace().lock();
        let vspace = (&mut *aspace) as *mut AddrSpace as usize;
        libvsched2::load_and_init(vspace);
        // TODO: load_and_init 不返回 vDSO 映射地址。
        // 后续需要从 libvsched2 获取实际映射信息来初始化元数据。
        0usize
    };

    // TODO: 记录 vDSO/vVAR 元数据（用于 UserData 等模块）。
    // 目前使用占位值。实际映射信息需要从 libvsched2 获取。
    let vvar_start = vdso_start;
    unsafe {
        VSCHED2_VVAR_START_PA =
            usize::from(axhal::mem::virt_to_phys(VirtAddr::from(vvar_start as usize)));
        VSCHED2_VVAR_SIZE = 0x1000;
        VSCHED2_VDSO_START_PA = VSCHED2_VVAR_START_PA;
        VSCHED2_VDSO_SIZE = 0;
    }

    // 步骤 2: 注册各个接口 trait 实现到 vsched2 的 vtable。
    // 采用 vDSO 模式：调用 libvsched2 的泛型 init_vtable_* 函数。
    //
    // 这些函数会在内部取出 trait 方法的函数指针（如 StarTask::state as usize），
    // 传给 libvsched2.so 中由 trait_interface! 生成的
    // `#[no_mangle] extern "C" fn init_vtable_*()` 函数，存入虚表。
    //
    // 至此，vsched2 调度循环中对 Task/Stack/Context/... 的方法调用
    // 都会通过虚表转发到本模块的实现。

    // 任务接口：统一线程与协程的状态、优先级、上下文保存与恢复行为
    libvsched2::init_vtable_Task::<StarTask>();

    // 栈分配接口：提供各地址空间内的栈分配与回收能力
    libvsched2::init_vtable_Stack::<StarryStack>();

    // 上下文/特权级切换接口：封装内核态/用户态以及地址空间切换
    libvsched2::init_vtable_Context::<StarryContext>();

    // Trap 处理接口：为同步 trap 获取或创建处理任务
    libvsched2::init_vtable_TrapHandle::<StarryTrapHandle>();

    // 多核接口：提供当前 CPU 标识
    libvsched2::init_vtable_SMP::<StarrySmp>();

    // 地址空间切换接口：vsched2 调度循环中实际使用的地址空间切换路径
    libvsched2::init_vtable_VSpace::<StarryVSpace>();

    // 用户态 vDSO 私有数据映射接口：内核→用户地址翻译
    libvsched2::init_vtable_UserData::<StarryUserData>();
}
