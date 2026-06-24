use core::arch::global_asm;

use super::{task::VschedTaskImpl, trapframe::UserTrapFrame};

global_asm!(
    r#"
.globl vsched2_trap_vector
.align 4
vsched2_trap_vector:
    // ---- Phase 1: swap to pre-save stack ----
    //
    // sp  = buffer_top (never modified until .Lsave_context)
    // t3  = buffer_base = sp - KERNEL_STACK (used as trap frame base)
    //
    // All user regs except t3 are saved on stack at sp - offset
    // (negative offsets, within 12-bit range).
    csrrw   sp, sscratch, sp

    // Save user t0..t4 at buffer_top - offset (negative, 12-bit range ✓)
    sd      t0, -296(sp)
    sd      t1, -304(sp)
    sd      t2, -312(sp)
    sd      t3, -320(sp)
    sd      t4, -328(sp)

    // Save old sscratch (= buffer_top) to SAVED_SSCRATCH for trap_entry
    la      t0, {saved_sscratch_ptr}
    sd      sp, 0(t0)              // SAVED_SSCRATCH = buffer_top

    // t4 holds buffer_base for trap frame accesses
    li      t4, {kernel_stack_size}
    sub     t4, sp, t4             // t4 = buffer_base (sp unchanged!)

    // StarryOS design: kernel pages are mapped in all user page tables,
    // so kernel code/data/stack are accessible without SATP switch.

    // Set SUM and MXR
    li      t1, 0x40000
    csrs    sstatus, t1        // SUM (bit 18): allow S-mode load/store on U pages
    li      t1, 0x80000
    csrs    sstatus, t1        // MXR (bit 19): allow S-mode execute on U pages

    // ---- Phase 2: detect vsched2 into_kernel special ecall ----

    csrr    t0, scause
    li      t1, 8
    bne     t0, t1, .Lrestore_regs
    li      t1, 0xdead
    bne     a7, t1, .Lrestore_regs
    li      a0, 2
    csrr    t0, sstatus
    srli    a1, t0, 8
    andi    a1, a1, 1
    xori    a1, a1, 1
    call    vsched2_direct_entry_stub
    j       .

.Lrestore_regs:
    // ---- Phase 3: restore user t0..t3, save to trap frame ----
    // t4 = buffer_base (frame base), sp = buffer_top

    ld      t0, -296(sp)
    sd      t0, 40(t4)             // tf.regs.t0 = user t0
    ld      t1, -304(sp)
    sd      t1, 48(t4)             // tf.regs.t1 = user t1
    ld      t2, -312(sp)
    sd      t2, 56(t4)             // tf.regs.t2 = user t2
    ld      t3, -320(sp)           // t3 = user t3 (saved by .Lsave_context @224)

    // Save user sp (old sp is in sscratch after the swap)
    csrr    t0, sscratch
    sd      t0, 16(t4)

    // Restore sscratch to buffer_top (sp was never modified)
    csrw    sscratch, sp

.Lsave_context:
    // ---- Phase 4: save full trap context (t4 = frame base) ----

    csrr    t0, sepc
    csrr    t1, sstatus
    csrr    t2, scause
    sd      t0, 256(t4)
    sd      t1, 264(t4)
    sd      t2, 272(t4)
    csrr    t0, stval
    sd      t0, 280(t4)

    sd      zero, 0(t4)
    sd      ra, 8(t4)
    sd      gp, 24(t4)                // save user gp
    la      t0, {kernel_gp_ptr}        // restore kernel gp (per-CPU base)
    ld      gp, 0(t0)
    sd      tp, 32(t4)                // save user tp
    sd      s0, 64(t4)
    sd      s1, 72(t4)
    sd      a0, 80(t4)
    sd      a1, 88(t4)
    sd      a2, 96(t4)
    sd      a3, 104(t4)
    sd      a4, 112(t4)
    sd      a5, 120(t4)
    sd      a6, 128(t4)
    sd      a7, 136(t4)
    sd      s2, 144(t4)
    sd      s3, 152(t4)
    sd      s4, 160(t4)
    sd      s5, 168(t4)
    sd      s6, 176(t4)
    sd      s7, 184(t4)
    sd      s8, 192(t4)
    sd      s9, 200(t4)
    sd      s10, 208(t4)
    sd      s11, 216(t4)
    sd      t3, 224(t4)               // user t3 (restored in Phase 3)
    // Restore user t4 from stack, then save to frame
    ld      t3, -328(sp)              // t3 = user t4
    sd      t3, 232(t4)               // tf.regs.t4 = user t4
    sd      t5, 240(t4)
    sd      t6, 248(t4)

    li      t0, 1
    sd      t0, 288(t4)

    // call stub
    csrr    t0, scause
    srli    a0, t0, 63           // a0 = trap_type
    li      a1, 0                // a1 = privilege (kernel mode)
    mv      a2, t4               // a2 = &UserTrapFrame
    li      a3, 0                // a3 = no task_ptr
    call    vsched2_trap_entry_stub
    j       .

"#,
    kernel_gp_ptr = sym crate::vsched::KERNEL_GP,
    saved_sscratch_ptr = sym crate::vsched::SAVED_SSCRATCH,
    kernel_stack_size = const crate::vsched::KERNEL_STACK,
);

#[unsafe(no_mangle)]
extern "C" fn vsched2_direct_entry_stub(trap_type: usize, privilege: usize) -> ! {
    let entry = unsafe {
        libvsched2::VDSO_VTABLE.raw_trap_entry.expect("raw_trap_entry not in vtable")
    };
    unsafe {
        core::arch::asm!(
            "mv a0, {t}", "mv a1, {p}", "jalr {f}",
            t = in(reg) trap_type, p = in(reg) privilege, f = in(reg) entry,
            options(noreturn),
        );
    }
    unreachable!()
}

#[unsafe(no_mangle)]
extern "C" fn vsched2_trap_entry_stub(
    trap_type: usize, privilege: usize,
    tf_stack: *const UserTrapFrame, task_ptr: *const (),
) -> ! {
    let tf = unsafe { &*tf_stack };
    if tf.scause == 0x8000000000000005 {
        unsafe { core::arch::asm!("li {t}, -1", "csrw stimecmp, {t}", t = out(reg) _); }
    }

    static TRAP_COUNT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    static mut TF_POOL: [UserTrapFrame; 4] = unsafe { core::mem::zeroed() };
    let trap_n = TRAP_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let idx = trap_n & 3;
    unsafe { core::ptr::copy_nonoverlapping(tf_stack, &raw mut TF_POOL[idx], 1) };
    let tf_ptr = unsafe { &raw mut TF_POOL[idx] as *mut UserTrapFrame };

    if trap_n < 200 {
        let tf = unsafe { &*tf_stack };
        let cur_sscratch: usize;
        unsafe { core::arch::asm!("csrr {}, sscratch", out(reg) cur_sscratch); }
        axlog::ax_println!(
            "[trap#{}] scause={:#x} sepc={:#x} stval={:#x} sscratch={:#x}",
            trap_n, tf.scause, tf.sepc, tf.stval, cur_sscratch,
        );
    }

    let mut task_ptr = task_ptr;
    if task_ptr.is_null() { task_ptr = libvsched2::current_task_ptr(); }
    if !task_ptr.is_null() {
        let vti = unsafe { &*(task_ptr as *const VschedTaskImpl) };
        vti.trap_frame.store(tf_ptr as usize, core::sync::atomic::Ordering::Release);
    }

    let entry = unsafe {
        libvsched2::VDSO_VTABLE.raw_trap_entry.expect("raw_trap_entry not in vtable")
    };
    let saved_sscratch = crate::vsched::SAVED_SSCRATCH.load(core::sync::atomic::Ordering::Acquire);
    unsafe {
        core::arch::asm!(
            "jalr {f}", f = in(reg) entry,
            in("a0") trap_type, in("a1") privilege, in("a2") saved_sscratch,
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
        "sd     ra, 8(sp)",  "addi   t0, sp, 296", "sd     t0, 16(sp)",
        "sd     s0, 64(sp)",  "sd     s1, 72(sp)",
        "sd     s2, 144(sp)", "sd     s3, 152(sp)", "sd     s4, 160(sp)",
        "sd     s5, 168(sp)", "sd     s6, 176(sp)", "sd     s7, 184(sp)",
        "sd     s8, 192(sp)", "sd     s9, 200(sp)", "sd     s10, 208(sp)",
        "sd     s11, 216(sp)", "li     t0, 0", "sd     t0, 288(sp)",
        "mv     a0, sp", "call   vsched_yield_entry_stub",
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
