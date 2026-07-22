//! 用户态 trap 帧结构定义及恢复函数。
//!
//! `UserTrapFrame` 布局与 `axcpu::TrapFrame` 对齐，包含 32 个通用寄存器
//! （含 zero）+ sepc / sstatus / scause / stval + kind（Yield / Trap）。
//!
//! ### 恢复路径概要
//!
//! | 函数 | 何时调用 | 特权级 | 使用 CSR |
//! |------|---------|--------|---------|
//! | `restore_and_sret` | `into_user_context` | S-mode | sepc, sstatus, sret |
//! | `restore_and_jump(Yield)` | yield 恢复 | U/S 均可 | 无 |
//! | `restore_and_jump(Trap)` | preemption 恢复 | **仅 S-mode** | 无 (sepc 从 tf offset 256 重读) |
//!
//! `restore_and_jump(Trap)` 恢复全部 31 GPR 后从 offset 256 重读 sepc
//! 跳转。GPR 恢复仅覆盖 offset 0~248，sepc 在 offset 256 处始终完好。无需暂存。

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UserGeneralRegs {
    pub zero: usize,
    pub ra: usize,
    pub sp: usize,
    pub gp: usize,
    pub tp: usize,
    pub t0: usize,
    pub t1: usize,
    pub t2: usize,
    pub s0: usize,
    pub s1: usize,
    pub a0: usize,
    pub a1: usize,
    pub a2: usize,
    pub a3: usize,
    pub a4: usize,
    pub a5: usize,
    pub a6: usize,
    pub a7: usize,
    pub s2: usize,
    pub s3: usize,
    pub s4: usize,
    pub s5: usize,
    pub s6: usize,
    pub s7: usize,
    pub s8: usize,
    pub s9: usize,
    pub s10: usize,
    pub s11: usize,
    pub t3: usize,
    pub t4: usize,
    pub t5: usize,
    pub t6: usize,
}

/// 区分 trap 帧的创建场景。
#[repr(usize)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UserTrapFrameKind {
    /// 主动让权，仅 callee-saved 寄存器有效。
    Yield = 0,
    /// 被中断/异常打断，全部 32 寄存器 + sepc / sstatus / scause / stval 有效。
    Trap  = 1,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UserTrapFrame {
    pub regs: UserGeneralRegs,
    pub sepc: usize,
    pub sstatus: usize,
    pub scause: usize,
    pub stval: usize,
    pub kind: UserTrapFrameKind,
}

impl UserTrapFrame {
    /// S-mode → U-mode 全量恢复。
    /// 写 sepc / sstatus，恢复全部 32 寄存器，执行 sret。
    /// 仅在内核态 `into_user_context` 路径下调用。
    #[cfg(target_arch = "riscv64")]
    pub unsafe fn restore_and_sret(&self) -> ! {
        unsafe {
            core::arch::asm!(
                "mv     sp, {tf}",
                "ld     t0, 256(sp)",
                "ld     t1, 264(sp)",
                "csrw   sepc, t0",
                "csrw   sstatus, t1",
                // 全量恢复 32 GPR
                "ld     ra, 8(sp)",
                "ld     gp, 24(sp)",
                "ld     tp, 32(sp)",
                "ld     t0, 40(sp)",
                "ld     t1, 48(sp)",
                "ld     t2, 56(sp)",
                "ld     s0, 64(sp)",
                "ld     s1, 72(sp)",
                "ld     a0, 80(sp)",
                "ld     a1, 88(sp)",
                "ld     a2, 96(sp)",
                "ld     a3, 104(sp)",
                "ld     a4, 112(sp)",
                "ld     a5, 120(sp)",
                "ld     a6, 128(sp)",
                "ld     a7, 136(sp)",
                "ld     s2, 144(sp)",
                "ld     s3, 152(sp)",
                "ld     s4, 160(sp)",
                "ld     s5, 168(sp)",
                "ld     s6, 176(sp)",
                "ld     s7, 184(sp)",
                "ld     s8, 192(sp)",
                "ld     s9, 200(sp)",
                "ld     s10, 208(sp)",
                "ld     s11, 216(sp)",
                "ld     t3, 224(sp)",
                "ld     t4, 232(sp)",
                "ld     t5, 240(sp)",
                "ld     t6, 248(sp)",
                "ld     sp, 16(sp)",
                "sret",
                tf = in(reg) self,
                options(noreturn),
            )
        }
    }

    /// 恢复全部寄存器，切换用户页表，并 sret 到用户态。
    /// 先在内核页表下恢复全部寄存器，然后切换 SATP，最后 sret。
    pub unsafe fn restore_and_sret_user(&self) -> ! {
        unsafe {
            core::arch::asm!(
                "mv     sp, {tf}",
                "ld     t0, 256(sp)",       // sepc → t0
                "ld     t1, 264(sp)",       // sstatus → t1
                "csrw   sepc, t0",
                "csrw   sstatus, t1",
                // 全量恢复 32 GPR（内核页表）
                "ld     ra, 8(sp)",
                "ld     gp, 24(sp)",
                "ld     tp, 32(sp)",
                "ld     t0, 40(sp)",
                "ld     t1, 48(sp)",
                "ld     t2, 56(sp)",
                "ld     s0, 64(sp)",
                "ld     s1, 72(sp)",
                "ld     a0, 80(sp)",
                "ld     a1, 88(sp)",
                "ld     a2, 96(sp)",
                "ld     a3, 104(sp)",
                "ld     a4, 112(sp)",
                "ld     a5, 120(sp)",
                "ld     a6, 128(sp)",
                "ld     a7, 136(sp)",
                "ld     s2, 144(sp)",
                "ld     s3, 152(sp)",
                "ld     s4, 160(sp)",
                "ld     s5, 168(sp)",
                "ld     s6, 176(sp)",
                "ld     s7, 184(sp)",
                "ld     s8, 192(sp)",
                "ld     s9, 200(sp)",
                "ld     s10, 208(sp)",
                "ld     s11, 216(sp)",
                "ld     t3, 224(sp)",
                "ld     t4, 232(sp)",
                "ld     t5, 240(sp)",
                "ld     t6, 248(sp)",
                "ld     sp, 16(sp)",
                "sret",
                tf = in(reg) self,
                options(noreturn),
            )
        }
    }

    /// 恢复全部寄存器并切到用户页表后 sret 到用户态。
    /// `satp_root` 已使用 satp::set 正确编码。
    #[deprecated = "use restore_and_sret_user + activate_user_aspace"]
    pub unsafe fn restore_and_sret_user_with_satp(&self, satp_root: usize) -> ! {
        unsafe {
            core::arch::asm!(
                "mv     sp, {tf}",
                // 加载 sepc/sstatus 并立即写入 CSR（值保持到 sret）
                "ld     t0, 256(sp)",       // sepc → t0
                "ld     t1, 264(sp)",       // sstatus → t1
                "csrw   sepc, t0",          // 写入 CSR, 不再需要 t0/t1
                "csrw   sstatus, t1",
                // 全量恢复 32 GPR（全部用内核页表，t0/t1 被覆盖但 CSR 已写）
                "ld     ra, 8(sp)",
                "ld     gp, 24(sp)",
                "ld     tp, 32(sp)",
                "ld     t0, 40(sp)",
                   "ld     t1, 48(sp)",
                "ld     t2, 56(sp)",
                "ld     s0, 64(sp)",
                "ld     s1, 72(sp)",
                "ld     a0, 80(sp)",
                "ld     a1, 88(sp)",
                "ld     a2, 96(sp)",
                "ld     a3, 104(sp)",
                "ld     a4, 112(sp)",
                "ld     a5, 120(sp)",
                "ld     a6, 128(sp)",
                "ld     a7, 136(sp)",
                "ld     s2, 144(sp)",
                "ld     s3, 152(sp)",
                "ld     s4, 160(sp)",
                "ld     s5, 168(sp)",
                "ld     s6, 176(sp)",
                "ld     s7, 184(sp)",
                "ld     s8, 192(sp)",
                "ld     s9, 200(sp)",
                "ld     s10, 208(sp)",
                "ld     s11, 216(sp)",
                "ld     t3, 224(sp)",
                "ld     t4, 232(sp)",
                "ld     t5, 240(sp)",
                "ld     t6, 248(sp)",
                // 最后恢复用户 sp
                "ld     sp, 16(sp)",
                // 切换用户页表（仅影响 sret 之后）
                "csrw   satp, {satp}",
                "sfence.vma",
                "li     t3, 0xffffffc010000000",
                "li     t4, 82",
                "sb     t4, 0(t3)",
                "sret",
                tf = in(reg) self,
                satp = in(reg) satp_root,
                options(noreturn),
            )
        }
    }

    /// 不切换特权级的上下文恢复。根据 `kind` 走不同分支。
    ///
    /// - **Yield**: 恢复 callee-saved（ra, sp, s0-s11），`ret` 跳回。
    ///   不碰 CSR，U/S 双模安全。
    /// - **Trap**: 全量 31 寄存器恢复，用 `sscratch` 暂存 sepc 以避免
    ///   污染任何 GPR，最后 `jr t0` 跳回。仅 S-mode 安全。
    pub unsafe fn restore_and_jump(&self) -> ! {
        match self.kind {
            UserTrapFrameKind::Yield => {
                #[cfg(target_arch = "riscv64")]
                unsafe {
                    core::arch::asm!(
                        "mv     sp, {tf}",
                        // 仅 callee-saved + ra + sp
                        "ld     ra, 8(sp)",
                        "ld     s0, 64(sp)",
                        "ld     s1, 72(sp)",
                        "ld     s2, 144(sp)",
                        "ld     s3, 152(sp)",
                        "ld     s4, 160(sp)",
                        "ld     s5, 168(sp)",
                        "ld     s6, 176(sp)",
                        "ld     s7, 184(sp)",
                        "ld     s8, 192(sp)",
                        "ld     s9, 200(sp)",
                        "ld     s10, 208(sp)",
                        "ld     s11, 216(sp)",
                        "ld     sp, 16(sp)",
                        "ret",
                        tf = in(reg) self,
                        options(noreturn),
                    )
                }
                #[cfg(not(target_arch = "riscv64"))]
                unimplemented!();
            }
            UserTrapFrameKind::Trap => {
                // 全量恢复 31 GPR + sp，最后从 trap frame 重读 sepc 跳转。
                // 不使用 sscratch 暂存 —— sepc 在 offset 256 处从不被覆盖，
                // 只需在 GPR 恢复后 ld 一次即可。避免 sscratch 污染导致
                // 嵌套异常时 trap 向量把 sepc 当预保存栈用。
                // U-mode 执行此处 csr 指令会 illegal instruction，
                // 将来支持用户态 preemption 时需改用不依赖 CSR 的方案。
                #[cfg(target_arch = "riscv64")]
                unsafe {
                    core::arch::asm!(
                        "mv     sp, {tf}",
                        // 全量恢复 31 GPR
                        "ld     ra, 8(sp)",
                        "ld     gp, 24(sp)",
                        "ld     tp, 32(sp)",
                        "ld     t0, 40(sp)",
                        "ld     t1, 48(sp)",
                        "ld     t2, 56(sp)",
                        "ld     s0, 64(sp)",
                        "ld     s1, 72(sp)",
                        "ld     a0, 80(sp)",
                        "ld     a1, 88(sp)",
                        "ld     a2, 96(sp)",
                        "ld     a3, 104(sp)",
                        "ld     a4, 112(sp)",
                        "ld     a5, 120(sp)",
                        "ld     a6, 128(sp)",
                        "ld     a7, 136(sp)",
                        "ld     s2, 144(sp)",
                        "ld     s3, 152(sp)",
                        "ld     s4, 160(sp)",
                        "ld     s5, 168(sp)",
                        "ld     s6, 176(sp)",
                        "ld     s7, 184(sp)",
                        "ld     s8, 192(sp)",
                        "ld     s9, 200(sp)",
                        "ld     s10, 208(sp)",
                        "ld     s11, 216(sp)",
                        "ld     t3, 224(sp)",
                        "ld     t4, 232(sp)",
                        "ld     t5, 240(sp)",
                        "ld     t6, 248(sp)",
                        // sepc 在 offset 256, 上面的 GPR 恢复从未碰它
                        "ld     t0, 256(sp)",
                        "ld     sp, 16(sp)",
                        "jr     t0",
                        tf = in(reg) self,
                        options(noreturn),
                    )
                }
                #[cfg(not(target_arch = "riscv64"))]
                unimplemented!();
            }
        }
    }
}
