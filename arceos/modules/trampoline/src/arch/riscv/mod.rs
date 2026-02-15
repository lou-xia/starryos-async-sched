use crate::trampoline;
use riscv::register::stvec;
use asynctask::TrapFrame;

/// Writes Supervisor Trap Vector Base Address Register (`stvec`).
#[inline]
pub fn set_trap_vector_base(stvec: usize) {
    unsafe { stvec::write(stvec, stvec::TrapMode::Direct) }
}

/// To initialize the trap vector base address.
pub fn init_interrupt() {
    set_trap_vector_base(trap_vector_base as usize);
}

#[unsafe(naked)]
#[unsafe(link_section = ".text")]
// #[repr(align(4))]
#[rustc_align(4)]
pub unsafe extern "C" fn trap_vector_base() {
    core::arch::naked_asm!(
        "
        csrrw   sp, sscratch, sp            // 交换 sp 以及 sscratch 寄存器
        bnez    sp, 1f                      // sscratch 寄存器不为 0，在用户态发生了 Trap
        
        csrr    sp, sscratch                // sscratch 寄存器为 0，在内核发生了 Trap
                                            // 此时 sscratch 寄存器上的内容为发生 Trap 时的栈指针
        addi    sp, sp, -{trapframe_size}   // 在当前的内核栈上预留出 TrapFrame 的空间
        SAVE_REGS                           // 保存通用寄存器、sepc、sstatus、sp、fs0、fs1
        mv      a0, sp
        li      a1, 1                       // 设置 a1 寄存器，表示是通过 trap 进入到 trampoline 的
        li      a2, 0                       // 设置 a2 寄存器，表示是在内核中发生的 Trap
        call    {trampoline}                // 调用 trampoline 处理中断
        RESTORE_REGS
        sret

        1:
        SAVE_REGS                           // sp 为 trapframe 的指针，sscratch 为之前的栈指针

        LDR     t1, sp, 2                   // load gp with CPU ID
        LDR     t0, sp, 3                   // load tp
        STR     gp, sp, 2                   // save gp and tp
        STR     tp, sp, 3
        mv      gp, t1
        mv      tp, t0

        li      a0, 1
        STR     a0, sp, 37                  // 表示任务需要优先处理 Trap
        mv      a0, sp                      // 传递 TrapFrame 的指针
        li      a1, 1                       // 设置 a0 寄存器，表示是通过 trap 进入到 trampoline 的
        li      a2, 1                       // 表示是由用户态进入
        LDR     sp, sp, 38                  // 从栈上加载 内核栈 的栈顶
        call    {trampoline}                // 调用 trampoline 处理中断
        // 当有任务在运行时，不会从这里返回，在 trampoline 中会调用 trap_frame 的 trap_return 直接返回
        ",
        trapframe_size = const core::mem::size_of::<TrapFrame>(),
        trampoline = sym trampoline,
    )
}

macro_rules! include_save_regs_macros {
    () => {
        core::arch::global_asm!(
            r"
            .macro SAVE_REGS
                PUSH_GENERAL_REGS                   // 保存通用寄存器

                csrr    t0, sepc
                csrr    t1, sstatus
                csrrw   t2, sscratch, zero          // save sscratch (sp) and zero it
                STR     t0, sp, 31                  // tf.sepc
                STR     t1, sp, 32                  // tf.sstatus
                STR     t2, sp, 1                   // tf.regs.sp
                .short  0xa622                      // fsd fs0,264(sp)
                .short  0xaa26                      // fsd fs1,272(sp)
                csrr    t0, scause                  
                csrr    t1, stval                   
                STR     t0, sp, 35                  // save scause
                STR     t1, sp, 36                  // save stval
                
                li      t0, 1
                STR     t0, sp, 37                  // update trap status
            .endm
            ",
        );
    };
}

macro_rules! include_restore_regs_macros {
    () => {
        core::arch::global_asm!(
            r"
            .macro RESTORE_REGS
                LDR     t0, sp, 31                  // load sepc from tf.sepc
                LDR     t1, sp, 32                  // load sstatus from tf.sstatus
                csrw    sepc, t0
                csrw    sstatus, t1
                .short  0x2432                      // fld fs0,264(sp)
                .short  0x24d2                      // fld fs1,272(sp)
                POP_GENERAL_REGS                    // 恢复通用寄存器
                LDR     sp, sp, 1                   // load sp from tf.regs.sp
            .endm
            ",
        );
    };
}

include_save_regs_macros!();
include_restore_regs_macros!();

macro_rules! include_asm_marcos {
    () => {
        #[cfg(target_arch = "riscv32")]
        core::arch::global_asm!(
            r"
        .ifndef XLENB
        .equ XLENB, 4

        .macro LDR rd, rs, off
            lw \rd, \off*XLENB(\rs)
        .endm
        .macro STR rs2, rs1, off
            sw \rs2, \off*XLENB(\rs1)
        .endm

        .endif"
        );

        #[cfg(target_arch = "riscv64")]
        core::arch::global_asm!(
            r"
        .ifndef XLENB
        .equ XLENB, 8

        .macro LDR rd, rs, off
            ld \rd, \off*XLENB(\rs)
        .endm
        .macro STR rs2, rs1, off
            sd \rs2, \off*XLENB(\rs1)
        .endm

        .endif",
        );

        core::arch::global_asm!(
            r"
        .ifndef .LPUSH_POP_GENERAL_REGS
        .equ .LPUSH_POP_GENERAL_REGS, 0

        .macro PUSH_POP_GENERAL_REGS, op
            \op ra, sp, 0
            \op t0, sp, 4
            \op t1, sp, 5
            \op t2, sp, 6
            \op s0, sp, 7
            \op s1, sp, 8
            \op a0, sp, 9
            \op a1, sp, 10
            \op a2, sp, 11
            \op a3, sp, 12
            \op a4, sp, 13
            \op a5, sp, 14
            \op a6, sp, 15
            \op a7, sp, 16
            \op s2, sp, 17
            \op s3, sp, 18
            \op s4, sp, 19
            \op s5, sp, 20
            \op s6, sp, 21
            \op s7, sp, 22
            \op s8, sp, 23
            \op s9, sp, 24
            \op s10, sp, 25
            \op s11, sp, 26
            \op t3, sp, 27
            \op t4, sp, 28
            \op t5, sp, 29
            \op t6, sp, 30
        .endm

        .macro PUSH_GENERAL_REGS
            PUSH_POP_GENERAL_REGS STR
        .endm
        .macro POP_GENERAL_REGS
            PUSH_POP_GENERAL_REGS LDR
        .endm

        .endif"
        );
    };
}

include_asm_marcos!();
