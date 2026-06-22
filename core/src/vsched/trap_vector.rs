use core::arch::global_asm;

use super::trapframe::UserTrapFrame;
use super::task::VschedTaskImpl;

global_asm!(
    r#"
.globl vsched2_trap_vector
.align 4
vsched2_trap_vector:
    // Step 0: save user t0 before clobbering (TRAP_SCRATCH)
    la      t2, {trap_scratch_ptr}
    sd      t0, 0(t2)

    // Step 1: load kernel SATP and switch
    ld      t0, {kernel_satp_ptr}  // load KERNEL_SATP_VAL (kernel data, user PT accessible)
    csrw    satp, t0
    sfence.vma zero, zero
    // Kernel PT is now active

    // Step 2: swap stack
    csrrw   sp, sscratch, sp

    // Step 3: restore user t0 and save scratch registers
    la      t2, {trap_scratch_ptr}
    ld      t0, 0(t2)              // t0 = user original t0
    sd      t0, 40(sp)             // save original t0 to tf.regs.t0
    sd      t1, 48(sp)             // save original t1
    sd      t2, 56(sp)             // save original t2

    // Step 4: save user sp, restore sscratch
    csrr    t0, sscratch
    sd      t0, 16(sp)
    csrw    sscratch, sp

    // Step 4: set SUM
    li      t1, 0x40000
    csrs    sstatus, t1

    // Detect vsched2 into_kernel ecall
    csrr    t0, scause
    li      t1, 8
    bne     t0, t1, .Lsave_context
    li      t1, 0xdead
    bne     a7, t1, .Lsave_context
    li      a0, 2
    csrr    t0, sstatus
    srli    a1, t0, 8
    andi    a1, a1, 1
    xori    a1, a1, 1
    call    vsched2_direct_entry_stub
    j       .

.Lsave_context:
    csrr    t0, sepc
    csrr    t1, sstatus
    csrr    t2, scause
    sd      t0, 256(sp)
    sd      t1, 264(sp)
    sd      t2, 272(sp)
    csrr    t0, stval
    sd      t0, 280(sp)

    sd      zero, 0(sp)
    sd      ra, 8(sp)
    sd      gp, 24(sp)                // save user gp
    la      t0, {kernel_gp_ptr}        // restore kernel gp (per-CPU base)
    ld      gp, 0(t0)
    sd      tp, 32(sp)                // save user tp
    sd      s0, 64(sp)
    sd      s1, 72(sp)
    sd      a0, 80(sp)
    sd      a1, 88(sp)
    sd      a2, 96(sp)
    sd      a3, 104(sp)
    sd      a4, 112(sp)
    sd      a5, 120(sp)
    sd      a6, 128(sp)
    sd      a7, 136(sp)
    sd      s2, 144(sp)
    sd      s3, 152(sp)
    sd      s4, 160(sp)
    sd      s5, 168(sp)
    sd      s6, 176(sp)
    sd      s7, 184(sp)
    sd      s8, 192(sp)
    sd      s9, 200(sp)
    sd      s10, 208(sp)
    sd      s11, 216(sp)
    sd      t3, 224(sp)
    sd      t4, 232(sp)
    sd      t5, 240(sp)
    sd      t6, 248(sp)

    li      t0, 1
    sd      t0, 288(sp)

    // call stub
    csrr    t0, scause
    srli    a0, t0, 63           // a0 = trap_type
    li      a1, 0                // a1 = privilege (kernel mode)
    mv      a2, sp               // a2 = &UserTrapFrame
    li      a3, 0                // a3 = no task_ptr (stub will use libvsched2::current_task_ptr)
    call    vsched2_trap_entry_stub
    j       .

"#,
    kernel_satp_ptr = sym crate::vsched::KERNEL_SATP_VAL,
    kernel_gp_ptr = sym crate::vsched::KERNEL_GP,
    trap_scratch_ptr = sym crate::vsched::TRAP_SCRATCH,
);

#[unsafe(no_mangle)]
extern "C" fn vsched2_direct_entry_stub(trap_type: usize, privilege: usize) -> ! {
    let entry = unsafe {
        libvsched2::VDSO_VTABLE.raw_trap_entry.expect("raw_trap_entry not in vtable")
    };
    unsafe {
        core::arch::asm!(
            "mv a0, {t}",
            "mv a1, {p}",
            "jalr {f}",
            t = in(reg) trap_type,
            p = in(reg) privilege,
            f = in(reg) entry,
            options(noreturn),
        );
    }
    unreachable!()
}

#[unsafe(no_mangle)]
extern "C" fn vsched2_trap_entry_stub(
    trap_type: usize,
    privilege: usize,
    tf_stack: *const UserTrapFrame,
    task_ptr: *const (),
) -> ! {
    // For timer interrupts, acknowledge by setting stimecmp to a future time
    let tf = unsafe { &*tf_stack };
    if tf.scause == 0x8000000000000005 {
        unsafe {
            core::arch::asm!("li {t}, -1", "csrw stimecmp, {t}", t = out(reg) _);
        }
    }

    // TF_STORAGE pool to avoid nested-trap overwrite
    static TRAP_COUNT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    static mut TF_POOL: [UserTrapFrame; 4] = unsafe { core::mem::zeroed() };
    let trap_n = TRAP_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let idx = trap_n & 3;
    unsafe { core::ptr::copy_nonoverlapping(tf_stack, &raw mut TF_POOL[idx], 1) };
    let tf_ptr = unsafe { &raw mut TF_POOL[idx] as *mut UserTrapFrame };

    // Diagnostic: print first 200 traps
    if trap_n < 200 {
        let tf = unsafe { &*tf_stack };
        axlog::ax_println!("[trap#{}] scause={:#x} sepc={:#x} stval={:#x}",
            trap_n, tf.scause, tf.sepc, tf.stval);
    }

    let mut task_ptr = task_ptr;
    if task_ptr.is_null() {
        task_ptr = libvsched2::current_task_ptr();
    }
    if !task_ptr.is_null() {
        let vti = unsafe { &*(task_ptr as *const VschedTaskImpl) };
        vti.trap_frame.store(tf_ptr as usize, core::sync::atomic::Ordering::Release);
    }

    let entry = unsafe {
        libvsched2::VDSO_VTABLE.raw_trap_entry.expect("raw_trap_entry not in vtable")
    };
    unsafe {
        core::arch::asm!(
            "jalr {f}",
            f = in(reg) entry,
            in("a0") trap_type,
            in("a1") privilege,
            in("a2") task_ptr,
            options(noreturn),
        );
    }
    unreachable!()
}

// ---- OS yield entry ----

#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn vsched_yield_trampoline() -> ! {
    core::arch::naked_asm!(
        "addi   sp, sp, -296",
        "sd     ra, 8(sp)",
        "addi   t0, sp, 296",
        "sd     t0, 16(sp)",
        "sd     s0, 64(sp)",
        "sd     s1, 72(sp)",
        "sd     s2, 144(sp)",
        "sd     s3, 152(sp)",
        "sd     s4, 160(sp)",
        "sd     s5, 168(sp)",
        "sd     s6, 176(sp)",
        "sd     s7, 184(sp)",
        "sd     s8, 192(sp)",
        "sd     s9, 200(sp)",
        "sd     s10, 208(sp)",
        "sd     s11, 216(sp)",
        "li     t0, 0",
        "sd     t0, 288(sp)",
        "mv     a0, sp",
        "call   vsched_yield_entry_stub",
    )
}

#[unsafe(no_mangle)]
extern "C" fn vsched_yield_entry_stub(tf_stack: *const UserTrapFrame) -> ! {
    let heap_tf = alloc::boxed::Box::new(unsafe { core::ptr::read(tf_stack) });
    let tf_ptr = alloc::boxed::Box::into_raw(heap_tf);

    let current_task = libvsched2::current_task_ptr();
    if !current_task.is_null() {
        let vti = unsafe { &*(current_task as *const VschedTaskImpl) };
        vti.trap_frame.store(tf_ptr as usize, core::sync::atomic::Ordering::Release);
    }

    let entry = unsafe {
        libvsched2::VDSO_VTABLE.raw_thread_entry.expect("raw_thread_entry not in vtable")
    };
    entry();
}
