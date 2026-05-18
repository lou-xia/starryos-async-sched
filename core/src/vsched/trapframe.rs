#[repr(C)]
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

#[repr(usize)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UserTrapFrameKind {
    Yield = 0,
    Trap = 1,
}

#[repr(C)]
pub struct UserTrapFrame {
    pub regs: UserGeneralRegs,
    pub sepc: usize,
    pub sstatus: usize,
    pub scause: usize,
    pub stval: usize,
    pub kind: UserTrapFrameKind,
}

impl UserTrapFrame {
    #[cfg(target_arch = "riscv64")]
    pub unsafe fn restore_and_sret(&self) -> ! {
        unsafe {
            core::arch::asm!(
                "mv     sp, {tf}",
                "ld     t0, 256(sp)",
                "ld     t1, 264(sp)",
                "csrw   sepc, t0",
                "csrw   sstatus, t1",
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

    pub unsafe fn restore_and_jump(&self) -> ! {
        match self.kind {
            UserTrapFrameKind::Yield => {
                #[cfg(target_arch = "riscv64")]
                unsafe {
                    core::arch::asm!(
                        "mv     sp, {tf}",
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
                {
                    unimplemented!("UserTrapFrame::restore_and_jump Yield: unsupported architecture");
                }
            }
            UserTrapFrameKind::Trap => {
                #[cfg(target_arch = "riscv64")]
                unsafe {
                    core::arch::asm!(
                        "mv     sp, {tf}",
                        "ld     t0, 256(sp)",
                        "csrw   sscratch, t0",
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
                        "csrr   t0, sscratch",
                        "ld     sp, 16(sp)",
                        "jr     t0",
                        tf = in(reg) self,
                        options(noreturn),
                    )
                }
                #[cfg(not(target_arch = "riscv64"))]
                {
                    unimplemented!("UserTrapFrame::restore_and_jump Trap: unsupported architecture");
                }
            }
        }
    }
}
