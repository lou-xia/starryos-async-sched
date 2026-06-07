use core::arch::global_asm;

use axhal::mem::phys_to_virt;
use memory_addr::PhysAddr;

use super::trapframe::UserTrapFrame;
use super::task::VschedTaskImpl;

global_asm!(
    r#"
.globl vsched2_trap_vector
.align 4
vsched2_trap_vector:
    // Step 1: swap sp ↔ sscratch. sscratch is always pre-set with a valid
    // pre-save stack (both user and kernel traps follow the same path).
    // sp ← old sscratch (pre-save stack), sscratch ← old sp
    csrrw   sp, sscratch, sp

    // Pre-save stack re-allocation is NOT done here — vsched2's trap_entry
    // calls set_pre_stack! for the next trap on every entry.
    // Hardware clears SUM on every trap; this single instruction ensures
    // the entire vsched2 dispatch chain sees user pages.
    csrs    sstatus, {sum}

    // Save scratch regs before they are clobbered by checks below.
    sd      t0, 40(sp)        // tf.regs.t0
    sd      t1, 48(sp)        // tf.regs.t1
    sd      t2, 56(sp)        // tf.regs.t2

    // Step 3: detect vsched2 into_kernel ecall (scause=8, a7=0xdead)
    // This path skips context save and goes directly to raw_trap_entry.
    csrr    t0, scause
    li      t1, 8
    bne     t0, t1, .Lsave_context
    li      t1, 0xdead
    bne     a7, t1, .Lsave_context
    // Special ecall: set trap_type=2, read privilege from sstatus.SPP
    li      a0, 2
    csrr    t0, sstatus
    srli    a1, t0, 8
    andi    a1, a1, 1
    call    vsched2_direct_entry_stub
    j       .

.Lsave_context:
    // Step 4: save full trap context into UserTrapFrame at sp.
    // t0, t1, t2 already saved above before they were clobbered.
    csrr    t0, sepc
    csrr    t1, sstatus
    csrr    t2, scause
    sd      t0, 256(sp)       // tf.sepc
    sd      t1, 264(sp)       // tf.sstatus
    sd      t2, 272(sp)       // tf.scause
    csrr    t0, stval
    sd      t0, 280(sp)       // tf.stval

    sd      zero, 0(sp)       // tf.regs.zero
    csrr    t0, sscratch       // saved sp (user sp or old kernel sp from csrrw)
    sd      t0, 8(sp)          // tf.regs.sp
    sd      ra, 16(sp)         // tf.regs.ra
    sd      gp, 24(sp)         // tf.regs.gp
    sd      tp, 32(sp)         // tf.regs.tp
    // t0,t1,t2 already saved
    sd      s0, 64(sp)         // tf.regs.s0
    sd      s1, 72(sp)         // tf.regs.s1
    sd      a0, 80(sp)         // tf.regs.a0
    sd      a1, 88(sp)         // tf.regs.a1
    sd      a2, 96(sp)         // tf.regs.a2
    sd      a3, 104(sp)        // tf.regs.a3
    sd      a4, 112(sp)        // tf.regs.a4
    sd      a5, 120(sp)        // tf.regs.a5
    sd      a6, 128(sp)        // tf.regs.a6
    sd      a7, 136(sp)        // tf.regs.a7
    sd      s2, 144(sp)        // tf.regs.s2
    sd      s3, 152(sp)        // tf.regs.s3
    sd      s4, 160(sp)        // tf.regs.s4
    sd      s5, 168(sp)        // tf.regs.s5
    sd      s6, 176(sp)        // tf.regs.s6
    sd      s7, 184(sp)        // tf.regs.s7
    sd      s8, 192(sp)        // tf.regs.s8
    sd      s9, 200(sp)        // tf.regs.s9
    sd      s10, 208(sp)       // tf.regs.s10
    sd      s11, 216(sp)       // tf.regs.s11
    sd      t3, 224(sp)        // tf.regs.t3
    sd      t4, 232(sp)        // tf.regs.t4
    sd      t5, 240(sp)        // tf.regs.t5
    sd      t6, 248(sp)        // tf.regs.t6

    li      t0, 1
    sd      t0, 288(sp)        // tf.kind = Trap

    // Step 5: a0 = trap_type (0=exception, 1=interrupt)
    csrr    t0, scause
    srli    a0, t0, 63         // MSB of scause: 1=interrupt, 0=exception

    // a1 = privilege (sstatus.SPP: 0=user, 1=supervisor)
    csrr    t0, sstatus
    srli    a1, t0, 8
    andi    a1, a1, 1

    // Step 6: call Rust stub with trap frame pointer
    mv      a2, sp             // a2 = &UserTrapFrame
    call    vsched2_trap_entry_stub
    j       .

"#,
    sum = const 1usize << 18,
);

#[unsafe(no_mangle)]
extern "C" fn vsched2_direct_entry_stub(_trap_type: usize, _privilege: usize) -> ! {
    let entry = unsafe {
        libvsched2::VDSO_VTABLE.raw_trap_entry.expect("raw_trap_entry not in vtable")
    };
    entry();
}

// ---- OS yield entry ----

#[unsafe(naked)]
unsafe extern "C" fn vsched_yield_trampoline() -> ! {
    core::arch::naked_asm!(
        "addi   sp, sp, -296",      // allocate UserTrapFrame on stack
        "sd     ra, 16(sp)",        // tf.regs.ra
        "addi   t0, sp, 296",       // caller's sp before addi
        "sd     t0, 8(sp)",         // tf.regs.sp
        "sd     s0, 64(sp)",        // tf.regs.s0 (fp)
        "sd     s1, 72(sp)",        // tf.regs.s1
        "sd     s2, 144(sp)",       // tf.regs.s2
        "sd     s3, 152(sp)",       // tf.regs.s3
        "sd     s4, 160(sp)",       // tf.regs.s4
        "sd     s5, 168(sp)",       // tf.regs.s5
        "sd     s6, 176(sp)",       // tf.regs.s6
        "sd     s7, 184(sp)",       // tf.regs.s7
        "sd     s8, 192(sp)",       // tf.regs.s8
        "sd     s9, 200(sp)",       // tf.regs.s9
        "sd     s10, 208(sp)",      // tf.regs.s10
        "sd     s11, 216(sp)",      // tf.regs.s11
        "li     t0, 0",
        "sd     t0, 288(sp)",       // tf.kind = Yield
        "mv     a0, sp",            // a0 = &UserTrapFrame
        "call   vsched_yield_entry_stub",
    )
}

#[unsafe(no_mangle)]
extern "C" fn vsched_yield_entry_stub(tf_stack: *const UserTrapFrame) -> ! {
    let heap_tf = alloc::boxed::Box::new(unsafe { core::ptr::read(tf_stack) });
    let tf_ptr = alloc::boxed::Box::into_raw(heap_tf);

    let vvar_base = phys_to_virt(PhysAddr::from(unsafe { crate::vsched::VSCHED2_VVAR_START_PA }));
    let cpu_id = axhal::percpu::this_cpu_id();
    let current_task: *mut () = unsafe {
        let ptr = (vvar_base.as_usize() + cpu_id * 8) as *const *mut ();
        ptr.read_volatile()
    };
    if !current_task.is_null() {
        let vti = unsafe { &*(current_task as *const VschedTaskImpl) };
        vti.trap_frame.store(tf_ptr as usize, core::sync::atomic::Ordering::Release);
    }

    let entry = unsafe {
        libvsched2::VDSO_VTABLE.raw_thread_entry.expect("raw_thread_entry not in vtable")
    };
    entry();
}

#[unsafe(no_mangle)]
extern "C" fn vsched2_trap_entry_stub(
    _trap_type: usize,
    _privilege: usize,
    tf_stack: *const UserTrapFrame,
) -> ! {
    let heap_tf = alloc::boxed::Box::new(unsafe { core::ptr::read(tf_stack) });
    let tf_ptr = alloc::boxed::Box::into_raw(heap_tf);

    // Store trap_frame in the CURRENT task (read from vsched2 VvarData).
    // CURRENT_TASK is at offset 0 in VvarData, per-CPU: [AtomicPtr<()>; CPU_NUM].
    let vvar_base = phys_to_virt(PhysAddr::from(unsafe { crate::vsched::VSCHED2_VVAR_START_PA }));
    let cpu_id = axhal::percpu::this_cpu_id();
    let current_task: *mut () = unsafe {
        let ptr = (vvar_base.as_usize() + cpu_id * 8) as *const *mut ();
        ptr.read_volatile()
    };
    if !current_task.is_null() {
        let vti = unsafe { &*(current_task as *const VschedTaskImpl) };
        vti.trap_frame.store(tf_ptr as usize, core::sync::atomic::Ordering::Release);
    }

    let entry = unsafe {
        libvsched2::VDSO_VTABLE.raw_trap_entry.expect("raw_trap_entry not in vtable")
    };
    entry();
    // raw_trap_entry returns ! (never returns, enters scheduling loop)
}
