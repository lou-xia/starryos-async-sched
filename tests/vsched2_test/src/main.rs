//! vsched2 vDSO 用户态集成测试。
//!
//! # dp：测试目标
//!
//! dp：本测试验证 vsched2 vDSO 的完整加载链路：
//! dp：1. 内核侧通过 `vdso_init()` 加载 `libvsched2.so` 并映射到用户地址空间
//! dp：2. 内核侧通过 AT_SYSINFO_EHDR (getauxval 33) 将 vDSO 基址传递给用户态
//! dp：3. 用户态调用 `init_vdso_vtable(base)` 初始化函数调用虚表
//! dp：4. 用户态通过虚表间接调用 vsched2 的 API 函数，验证数据正确性
//!
//! # dp：测试内容
//!
//! dp：- API 版本检查：验证 vDSO 加载与 vtable 初始化成功
//! dp：- 调度器状态查询：读取 CURRENT_TASK / IN_KERNEL / CURRENT_VSPACE
//! dp：- 架构演示：以注释形式展示多协程并发的完整流程（等后续步骤集成后启用）

// dp：Linux getauxval(3) 系统调用包装。用于获取 AT_SYSINFO_EHDR (type=33)，
// dp：即 vDSO 在用户地址空间中的基地址。
unsafe extern "C" {
    fn getauxval(type_: u64) -> u64;
}

/// dp：AT_SYSINFO_EHDR — 指向 vDSO ELF header 的基地址。
const AT_SYSINFO_EHDR: u64 = 33;

fn main() {
    println!("=== vsched2 vDSO Integration Test ===");

    // =====================================================================
    // dp：步骤 1 — 获取 vDSO 基址（由内核在加载进程时通过 auxv 传递）
    // dp：若返回 0，说明内核未正确设置 AT_SYSINFO_EHDR 或 vDSO 未映射。
    // =====================================================================
    let vdso_base = unsafe { getauxval(AT_SYSINFO_EHDR) };
    assert_ne!(vdso_base, 0, "vDSO base is null — kernel did not set AT_SYSINFO_EHDR");
    println!("[PASS] vDSO base: 0x{:016x}", vdso_base);

    // =====================================================================
    // dp：步骤 2 — 初始化用户侧的 vDSO 函数调用虚表。
    // dp：init_vdso_vtable 会扫描 .dynsym 段，将 vsched2 导出的函数地址
    // dp：填入全局 VDSO_VTABLE，后续 API 调用通过虚表间接跳转到 .so 中的函数体。
    // =====================================================================
    unsafe {
        libvsched2::init_vdso_vtable(vdso_base);
    }
    println!("[PASS] vDSO vtable initialized");

    // =====================================================================
    // dp：步骤 3 — 调用 vsched2 API 函数验证加载正确性
    // =====================================================================

    // dp：3.1 — 版本检查（核心验证点：vDSO 代码段已正确映射并可执行）
    let version = libvsched2::vsched2_version();
    let expected = 0x0001_0000;
    assert_eq!(
        version, expected,
        "vsched2_version returned {:#x}, expected {:#x} — vDSO loading may have failed",
        version, expected
    );
    println!("[PASS] vsched2_version() = {:#x} (expected {:#x})", version, expected);

    // dp：3.2 — CPU 0 的调度器状态查询（验证 VvarData 共享数据区可访问）
    let cpu_id = 0;

    let has_task = libvsched2::vsched2_has_current_task(cpu_id);
    let in_kernel = libvsched2::vsched2_is_in_kernel(cpu_id);
    let vspace = libvsched2::vsched2_current_vspace(cpu_id);
    let task_ptr = libvsched2::vsched2_current_task_ptr(cpu_id);

    println!("[INFO] CPU {} state:", cpu_id);
    println!("[INFO]   has_current_task:  {}", has_task);
    println!("[INFO]   is_in_kernel:      {}", in_kernel);
    println!("[INFO]   current_vspace:    {}", vspace);
    println!("[INFO]   current_task_ptr:  0x{:016x}", task_ptr);

    // dp：3.3 — 验证默认状态：系统初始化后，内核调度器应已创建但无运行任务
    assert!(!has_task, "No task should be running before scheduler starts");
    println!("[PASS] Scheduler state is consistent with post-init phase");

    // =====================================================================
    // dp：步骤 4 — 多协程并发调度演示（架构说明）
    // dp：
    // dp：以下注释展示了 vsched2 完整运行后的协程调度模型。
    // dp：待后续步骤完成 Context::into_kernel / into_user 等 trait 实现，
    // dp：以及 raw_kschedule 入口集成到内核 trap 处理路径后，即可启用。
    // dp：
    // dp：伪代码示例：
    // dp：
    // dp：    // 创建 3 个协程任务，优先级从高到低
    // dp：    let task_a = register_coroutine(priority: 1, || {
    // dp：        println!("coroutine A: step 1");  yield;
    // dp：        println!("coroutine A: step 2");  yield;
    // dp：        println!("coroutine A: done");
    // dp：    });
    // dp：    let task_b = register_coroutine(priority: 5, || {
    // dp：        println!("coroutine B: step 1");  yield;
    // dp：        println!("coroutine B: done");
    // dp：    });
    // dp：    let task_c = register_coroutine(priority: 10, || {
    // dp：        println!("coroutine C: step 1");  yield;
    // dp：        println!("coroutine C: done");
    // dp：    });
    // dp：
    // dp：    // 预期执行顺序（优先级调度）：
    // dp：    //   coroutine A: step 1   (prio 1, 最先)
    // dp：    //   coroutine B: step 1   (prio 5)
    // dp：    //   coroutine C: step 1   (prio 10)
    // dp：    //   coroutine A: step 2   (prio 1)
    // dp：    //   coroutine B: done     (prio 5)
    // dp：    //   coroutine C: done     (prio 10)
    // dp：    //   coroutine A: done     (prio 1)
    // dp：
    // dp：    // 启动调度器并运行
    // dp：    raw_kschedule();
    // dp：
    // dp：当前状态：vDSO 加载和虚表初始化已完成（本测试验证的内容）。
    // dp：下一步：实现 Context trait 的方法，集成 raw_kschedule 到 trap 路径。
    // =====================================================================

    println!();
    println!("=== ALL TESTS PASSED ===");
    println!();
    println!("Summary:");
    println!("  - vDSO loading:              OK");
    println!("  - Vtable initialization:     OK");
    println!("  - API function dispatch:     OK");
    println!("  - VvarData accessibility:    OK");
    println!();
    println!("Next steps (for future integration):");
    println!("  1. Implement Context::into_kernel / into_user / into_user_context");
    println!("  2. Integrate raw_kschedule entry to kernel trap handler");
    println!("  3. Enable the coroutine scheduling demo above");
}
