# vsched2 接口适配层设计文档

## 概述

`vsched2` 是一个可移植的统一任务调度模块，通过 vDSO（virtual dynamic shared object）机制暴露调度接口。
StarryOS 通过 `libvsched2` 包装库（由 `build_vdso` 工具自动生成）来加载 vsched2 的 .so 并注册接口实现。

**核心模块**：
- **`vdso/`** — vDSO 加载器示例（`vdso_example`），展示如何通过 `build_vdso` 生成和使用 vDSO
- **`core/src/vsched.rs`** — vsched2 接口适配层，为 vsched2 的各个 trait_interface 提供 StarryOS 实现
- **`vdso_vsched2_output/libvsched2/`** — 由 `vdso/build.rs` 调用 `build_vdso` 自动生成的包装库
- **`vsched/vsched2/`** — vsched2 核心调度库（外部依赖，非 workspace 成员）
- **`vsched/vsched2/mydocs/接口文档.md`** — vsched2 的完整接口规范文档

## vDSO 模式

vDSO 是一种将内核代码编译为 `.so` 共享库的技术，运行时动态加载到指定地址空间。
`build_vdso` 工具会自动为 `.so` 中的 trait_interface 生成 Rust 包装代码，
外部模块（如 `core/src/vsched.rs`）通过实现 trait 并调用 `init_vtable_*` 将函数指针注册到 vsched2 的虚表中。

### 初始化流程

```
init_vsched2_interfaces()                    // core/src/vsched.rs
  ├─ libvsched2::load_and_init(vspace)       // 加载 .so 并初始化内核侧 vtable
  ├─ libvsched2::init_vtable_Task::<StarTask>()       // 注册 Task 接口
  ├─ libvsched2::init_vtable_Stack::<StarryStack>()   // 注册 Stack 接口
  ├─ libvsched2::init_vtable_Context::<StarryContext>() // 注册 Context 接口
  ├─ libvsched2::init_vtable_TrapHandle::<StarryTrapHandle>()
  ├─ libvsched2::init_vtable_SMP::<StarrySmp>()
  ├─ libvsched2::init_vtable_VSpace::<StarryVSpace>()
  └─ libvsched2::init_vtable_UserData::<StarryUserData>()
```

### 与旧方案（AI 生成代码）的对比

| 方面 | 旧方案（extern "C" 直接调用） | 新方案（vDSO 模式） |
|------|-------------------------------|---------------------|
| 接口注册 | `extern "C" fn init_vtable_Task(state: usize, ...)` 传递裸函数指针 | `init_vtable_Task::<StarTask>()` 泛型调用，编译器检查 |
| 任务映射 | 维护全局 `TASK_METADATA` HashMap 映射任务 ID → 元数据 | StarTask 直接包装 AxTaskRef，元数据内嵌 |
| 类型安全 | 无编译期检查，函数指针透传 | trait 实现由编译器验证完整性 |
| 模式一致性 | 不一致（直接 FFI） | 与 vdso/src/lib.rs 一致 |
| set_state 语义 | 返回新状态 | 返回**旧状态**（符合接口文档 §3.1） |
| TrapHandle 回退 | 返回原 task | 通过工厂函数获取专用 trap 处理任务 |

---

## Trait 接口清单及实现

vsched2 通过 `trait_interface!` 宏定义了以下接口 trait（对应 `vsched2/src/interface.rs`）。
详细规范请参考 `vsched/vsched2/mydocs/接口文档.md`。

### Task — 任务接口

**作用**：抽象外部任务对象，统一线程/协程的状态、优先级、上下文保存与恢复行为。

**StarryOS 适配器**：`StarTask` — 包装 `AxTaskRef` + 内嵌原子元数据字段

| 方法 | 说明 | StarryOS 实现 |
|------|------|---------------|
| `state(&self) -> TaskState` | 获取当前任务状态 | 映射 `axtask::TaskState` → `vsched2::TaskState` |
| `set_state(&self, state) -> TaskState` | 设置状态，**返回旧状态** | 先读取旧状态，再写入新状态 |
| `priority(&self) -> isize` | 获取优先级（值越小越高，范围 0..=15） | 读取 AtomicIsize 字段 |
| `is_coroutine(&self) -> bool` | 判断协程/线程 | 读取 AtomicBool 字段 |
| `pid(&self) -> usize` | 获取所属进程号（PROCESS_INFO_TABLE 索引） | 读取 AtomicUsize 字段 |
| `set_pid(&self, pid)` | 设置进程号（trap 处理任务继承用） | 写入 AtomicUsize 字段 |
| `save_thread_context(&self)` | 保存线程上下文（协程让权后调用） | 将 axtask 状态设为 Ready |
| `save_trap_context(&self)` | 保存 trap 上下文（预留接口） | 将 axtask 状态设为 Blocked |
| `restore_context(&self)` | 恢复寄存器上下文（线程被调度时调用） | **panic**（待 vsched2 上下文切换集成） |
| `poll(&self) -> Poll<usize>` | 协程轮询 | 转发到 `CoroutinePoll::poll()` |
| `thread_stack_base(&self) -> usize` | 获取线程栈底地址 | 从 `AxTaskRef::kernel_stack_top()` 计算 |
| `set_return_value(&self, value)` | 写入协程返回值 | 写入 AtomicUsize 字段 |

**关于 AxTask 作为调度对象**：
- 直接包装 `AxTaskRef`（Arc<AxTask>），不做外部分离的 task_metadata 映射表
- 原因：vsched2 中 Task trait 的方法签名为 `&self`，AxTaskRef 天然匹配
- 内嵌原子字段避免了额外 HashMap 查找开销
- ptr 生命周期：Box::leak 为 'static 后传裸指针给 vsched2

### Stack — 栈分配接口

**作用**：向调度模块提供各地址空间内的栈分配与回收能力。

**StarryOS 适配器**：`StarryStack`

| 方法 | 说明 | StarryOS 实现 |
|------|------|---------------|
| `alloc() -> *mut ()` | 分配栈（满足 16 字节对齐） | 全局分配器分配 KERNEL_STACK_SIZE 大小的栈 |
| `dealloc(stack: *mut ())` | 回收栈（调用前需确保不再被使用） | 全局分配器释放 |

后续可替换为专用栈池管理器（参考 vsched2 的 StackHandler/KERNEL_STACKS）。

### Context — 上下文/特权级切换接口

**作用**：封装内核态、用户态以及地址空间相关的底层切换行为。

**StarryOS 适配器**：`StarryContext`

| 方法 | 说明 | StarryOS 实现 |
|------|------|---------------|
| `into_kernel() -> !` | 从用户态调度器主动陷入内核 | **panic**（待集成） |
| `into_user(ustack)` | 内核→用户协程（传入用户栈顶 sp） | **panic**（待集成） |
| `into_user_context(task)` | 内核→用户线程（传入任务指针） | **panic**（待集成） |
| `switch_vspace(vspace)` | 切换地址空间 | 读取 AddrSpace 页表根，通过 asm 写入 satp |

注意（接口文档 §3.6）：真正被调度循环调用的是 `VSpace::into_vspace`，`Context::switch_vspace` 目前更像预留接口。

### TrapHandle — Trap 处理接口

**作用**：为同步 trap 获取或创建一个专门的 trap 处理任务。

**StarryOS 适配器**：`StarryTrapHandle`（通过全局工厂函数）

| 方法 | 说明 | StarryOS 实现 |
|------|------|---------------|
| `get_handler(task) -> *const ()` | 获取 trap 处理任务（不应返回空） | 调用注册的工厂函数；未注册则 panic |

通过 `register_trap_handler_factory()` 注册工厂：
- 输入：被 trap 的原任务指针
- 输出：trap 处理任务指针（新任务，非原任务）

### SMP — 多核接口

**作用**：向调度器提供当前 CPU 标识（用于 per-CPU 共享变量访问）。

**StarryOS 适配器**：`StarrySmp`

| 方法 | 说明 | StarryOS 实现 |
|------|------|---------------|
| `cpu_id() -> usize` | 获取当前 CPU ID（范围 0..CPU_NUM） | `axhal::percpu::this_cpu_id()` |

### VSpace — 地址空间切换接口（主调度路径）

**作用**：vsched2 调度循环中实际使用的地址空间切换路径。

**StarryOS 适配器**：`StarryVSpace`

| 方法 | 说明 | StarryOS 实现 |
|------|------|---------------|
| `into_vspace(vspace: *mut ())` | 切换到指定地址空间 | 与 Context::switch_vspace 相同逻辑 |

参数来自 `ProcessInfo.vspace`，约定指向 `AddrSpace`。

### UserData — vDSO 私有数据映射接口

**作用**：将内核侧 vVAR 数据区的内核虚拟地址翻译为用户地址空间中的对应地址。

**StarryOS 适配器**：`StarryUserData`

| 方法 | 说明 | StarryOS 实现 |
|------|------|---------------|
| `get_user_data(pos, len) -> *mut ()` | 内核→用户虚拟地址翻译 | 通过页表遍历查找目标物理页在用户空间中的映射地址 |

**安全约束**（接口文档 §3.7、§8.3）：
- 返回地址必须位于用户态 vDSO 私有数据区内
- `[addr, addr + len)` 必须完整可访问
- **切换地址空间前后不能继续使用同一份映射引用**

---

## 指针语义（§8.2）

接口中大量使用 `*const ()` 或 `*mut ()`，本质上是"类型擦除后的外部对象指针"：
- 调度模块不拥有这些对象的具体类型定义
- 外部实现必须保证生命周期和地址有效性
- StarTask 通过 Box::leak 转为 'static 引用，满足该约束

## 地址空间切换约束（§8.3）

- `CURRENT_VSPACE` 记录的是"当前或即将进入"的地址空间所属进程号
- 切换地址空间后，不应继续使用之前从 `UserData::get_user_data()` 获得的引用
- trap 处理任务若需要访问原任务地址空间，应保证 pid 已正确设置

## 未完成事项（TODO）

| 事项 | 优先级 | 说明 |
|------|--------|------|
| vsched2 vDSO/vVAR 元数据获取 | 高 | 当前 `load_and_init` 不返回映射地址，元数据为占位值 |
| 上下文切换集成 | 高 | `into_kernel`、`into_user`、`into_user_context`、`restore_context` 尚未接入 |
| 栈池管理 | 中 | 替换全局分配器为专用栈池 |
| 协程支持 | 中 | CoroutinePoll trait 已定义，协程创建和管理机制待建立 |
| 优先级联动 | 低 | 当前 priority 通过 register_task 设置，未与 axsched 调度优先级联动 |
| 用户态普通同步 trap 路径 | 中 | vsched2 当前 `trap_type==0 && privilege==1` 会触发 `unimplemented!` |
| 事件源公开注册接口 | 低 | `Scheduler::register_event_source` 已实现但为私有方法 |
