use riscv::register::{
    scause::{Exception, Interrupt, Trap},
    sstatus::{self, Sstatus},
};

/// General registers of RISC-V.
#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GeneralRegisters {
    pub ra: usize,
    pub sp: usize,
    pub gp: usize, // only valid for user traps
    pub tp: usize, // only valid for user traps
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

/// 用于表示内核处理是否处理完成，若处理完，则表示可以进入下一个阶段
#[repr(usize)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TrapStatus {
    #[default]
    Done    = 0,
    Blocked = 1,
    Unknown,
}

impl From<usize> for TrapStatus {
    #[inline]
    fn from(nr: usize) -> Self {
        match nr {
            0 => Self::Done,
            1 => Self::Blocked,
            _ => Self::Unknown,
        }
    }
}

impl TryFrom<TrapStatus> for usize {
    type Error = TrapStatus;

    #[inline]
    fn try_from(value: TrapStatus) -> Result<Self, Self::Error> {
        match value {
            TrapStatus::Unknown => Err(Self::Error::Unknown),
            _ => Ok(value as Self),
        }
    }
}

impl TrapFrame {
    /// 用于创建用户态任务的初始化
    pub fn init_user_context(app_entry: usize, user_sp: usize) -> Self {
        let sstatus = sstatus::read();
        // 当前版本的riscv不支持使用set_spp函数，需要手动修改
        // 修改当前的sstatus为User，即是第8位置0
        let mut trap_frame = TrapFrame::default();
        trap_frame.set_user_sp(user_sp);
        trap_frame.sepc = app_entry;
        trap_frame.sstatus =
            unsafe { (*(&sstatus as *const Sstatus as *const usize) & !(1 << 8)) & !(1 << 1) };
        unsafe {
            // a0为参数个数
            // a1存储的是用户栈底，即argv
            trap_frame.regs.a0 = *(user_sp as *const usize);
            trap_frame.regs.a1 = *(user_sp as *const usize).add(1);
        }
        trap_frame.scause = 0;
        trap_frame.stval = 0;
        trap_frame
    }

    pub fn set_user_sp(&mut self, user_sp: usize) {
        self.regs.sp = user_sp;
    }

    /// 设置返回值
    pub fn set_ret_code(&mut self, ret_value: usize) {
        self.regs.a0 = ret_value;
    }

    /// 设置TLS
    pub fn set_tls(&mut self, tls_value: usize) {
        self.regs.tp = tls_value;
    }

    /// 获取 sp
    pub fn get_sp(&self) -> usize {
        self.regs.sp
    }

    /// 设置 pc
    pub fn set_pc(&mut self, pc: usize) {
        self.sepc = pc;
    }

    /// pc 倒退到 syscall 指令的长度
    pub fn rewind_pc(&mut self) {
        self.sepc -= 4;
    }

    /// 设置 arg0
    pub fn set_arg0(&mut self, arg: usize) {
        self.regs.a0 = arg;
    }

    /// 设置 arg1
    pub fn set_arg1(&mut self, arg: usize) {
        self.regs.a1 = arg;
    }

    /// 设置 arg2
    pub fn set_arg2(&mut self, arg: usize) {
        self.regs.a2 = arg;
    }

    /// 获取 pc
    pub fn get_pc(&self) -> usize {
        self.sepc
    }

    /// 获取 ret
    pub fn get_ret_code(&self) -> usize {
        self.regs.a0
    }

    /// 设置返回地址
    pub fn set_ra(&mut self, ra: usize) {
        self.regs.ra = ra;
    }

    /// 获取所有 syscall 参数
    pub fn get_syscall_args(&self) -> [usize; 6] {
        [
            self.regs.a0,
            self.regs.a1,
            self.regs.a2,
            self.regs.a3,
            self.regs.a4,
            self.regs.a5,
        ]
    }

    /// 获取 syscall id
    pub fn get_syscall_num(&self) -> usize {
        self.regs.a7 as _
    }

    pub fn get_scause_type(&self) -> Trap {
        let is_interrupt = self.scause & (1 << (usize::BITS as usize - 1)) != 0;
        let code = self.scause & !(1 << (usize::BITS as usize - 1));
        if is_interrupt {
            Trap::Interrupt(Interrupt::from(code))
        } else {
            Trap::Exception(Exception::from(code))
        }
    }

    pub fn get_scause_code(&self) -> usize {
        self.scause
    }
}

impl TrapFrame {
    /// 只在内核中发生抢占时，恢复被打断的内核执行流时使用，不会在返回用户态时使用
    /// 不需要恢复 gp、tp 等寄存器
    #[unsafe(naked)]
    pub extern "C" fn preempt_return(&self) {
        core::arch::naked_asm!(
            r#"
                mv      sp, a0
                .short  0x2432                      // fld fs0,264(sp)
                .short  0x24d2                      // fld fs1,272(sp)

                LDR     t0, sp, 31
                LDR     t1, sp, 32
                csrw    sepc, t0
                csrw    sstatus, t1
                POP_GENERAL_REGS
                LDR     sp, sp, 1
                sret
                "#,
        );
    }

    /// 用于返回用户态执行流
    #[unsafe(naked)]
    pub unsafe extern "C" fn user_return(&self) {
        core::arch::naked_asm!(
            r#"
            mv      sp, a0
            .short  0x2432                      // fld fs0,264(sp)
            .short  0x24d2                      // fld fs1,272(sp)
            LDR     t0, sp, 2
            STR     gp, sp, 2
            mv      gp, t0
            LDR     t0, sp, 3
            STR     tp, sp, 3
            mv      tp, t0

            csrw    sscratch, a0

            LDR     t0, sp, 31
            LDR     t1, sp, 32
            csrw    sepc, t0
            csrw    sstatus, t1
            POP_GENERAL_REGS
            LDR     sp, sp, 1
            sret
            "#,
        );
    }
}

#[cfg(feature = "thread")]
impl TrapFrame {
    #[unsafe(naked)]
    pub extern "C" fn thread_ctx(set_tf_fn: usize, ctx_type: crate::CtxType) -> &'static Self {
        core::arch::naked_asm!(
            "
                addi    sp, sp, -{trap_frame_size}
                STR     ra, sp, 0
                STR     sp, sp, 1
                STR     s0, sp, 7
                STR     s1, sp, 8
                STR     s2, sp, 17
                STR     s3, sp, 18
                STR     s4, sp, 19
                STR     s5, sp, 20
                STR     s6, sp, 21
                STR     s7, sp, 22
                STR     s8, sp, 23
                STR     s9, sp, 24
                STR     s10, sp, 25
                STR     s11, sp, 26
                mv      ra, a0
                mv      a0, sp
                ret
                ",
            trap_frame_size = const core::mem::size_of::<TrapFrame>(),
        )
    }

    #[unsafe(naked)]
    pub extern "C" fn thread_return(&self) {
        core::arch::naked_asm!(
            "
                mv      sp, a0
                LDR     ra, sp, 0
                LDR     s0, sp, 7
                LDR     s1, sp, 8
                LDR     s2, sp, 17
                LDR     s3, sp, 18
                LDR     s4, sp, 19
                LDR     s5, sp, 20
                LDR     s6, sp, 21
                LDR     s7, sp, 22
                LDR     s8, sp, 23
                LDR     s9, sp, 24
                LDR     s10, sp, 25
                LDR     s11, sp, 26

                // 恢复 sp
                LDR     sp, sp, 1
                addi    sp, sp, {trap_frame_size}
                ret
                ",
            trap_frame_size = const core::mem::size_of::<TrapFrame>(),
        )
    }
}
