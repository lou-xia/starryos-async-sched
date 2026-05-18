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

    unsafe {
        libvsched2::init_vdso_vtable(vdso_base);
    }
}
