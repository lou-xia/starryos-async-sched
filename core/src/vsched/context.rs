//! VschedContextImpl / VschedVSpaceImpl — 特权级切换与地址空间切换接口实现。

use core::sync::atomic::{AtomicUsize, Ordering};

use axhal::mem::phys_to_virt;
use axmm::AddrSpace;
use libvsched2;
use memory_addr::PhysAddr;

use super::{task::VschedTaskImpl, trapframe::UserTrapFrame};

pub struct VschedContextImpl;
pub struct VschedVSpaceImpl;

const CPU_NUM: usize = axconfig::plat::CPU_NUM;

/// 将用户页表根写入 SATP，并刷新 TLB、设置 SUM 位。
pub fn activate_user_aspace(root: PhysAddr) {
    let current_root = axhal::asm::read_user_page_table();
    if current_root != root {
        unsafe {
            axhal::asm::write_user_page_table(root);
            axhal::asm::flush_tlb(None);
            #[cfg(target_arch = "riscv64")]
            core::arch::asm!("csrs sstatus, {}", in(reg) 1usize << 18);
        };
    }
}

/// 从 vsched2 传入的裸指针提取 `AddrSpace` 的页表根物理地址。
pub fn page_table_root_from_raw(ptr: *const ()) -> Option<PhysAddr> {
    if ptr.is_null() {
        return None;
    }
    let root = unsafe { &*(ptr as *const AddrSpace) }.page_table_root();
    if root.as_usize() == 0 {
        return None;
    }
    Some(root)
}

/// vsched2 用户调度器主动陷入内核时使用的特殊系统调用号。
pub const VSCHED2_INTO_KERNEL_SYSNO: usize = 0xdead;

/// `raw_run_task` 在 .so 中的偏移量。
static RAW_RUN_TASK_OFFSET: AtomicUsize = AtomicUsize::new(0);
/// 当前地址空间的 vDSO 基址，`into_vspace` 切换时写入，`get_user_data` 读取。
/// per-CPU 数组，与 vsched2 的 `CURRENT_VSPACE[CPU_NUM]` 模式一致。
pub static CURRENT_VDSO_BASE: [AtomicUsize; CPU_NUM] = [const { AtomicUsize::new(0) }; CPU_NUM];

/// 在 `init_vsched2_interfaces` 中调用，计算 `raw_run_task` 的 .so 内偏移。
pub fn init_raw_run_task_offset() {
    let kernel_addr = unsafe {
        libvsched2::VDSO_VTABLE
            .raw_run_task
            .expect("raw_run_task not in vtable") as usize
    };
    let kernel_vdso_base = phys_to_virt(PhysAddr::from(unsafe {
        crate::vsched::VSCHED2_VDSO_START_PA
    }))
    .as_usize();
    RAW_RUN_TASK_OFFSET.store(kernel_addr - kernel_vdso_base, Ordering::Release);
}

/// `handle_syscall` 拦截到特殊 ecall 时调用的桥接函数。
pub fn enter_raw_trap_entry() -> ! {
    let entry = unsafe {
        libvsched2::VDSO_VTABLE
            .raw_trap_entry
            .expect("raw_trap_entry not in vtable")
    };
    unsafe {
        core::arch::asm!(
            "li a0, 2",
            "li a1, 1",
            "jalr {f}",
            f = in(reg) entry,
            options(noreturn),
        );
    }
    unreachable!()
}

impl libvsched2::Context for VschedContextImpl {
    /// 用户态调度器 → 内核态：发起 ecall (a7=0xdead)。
    fn into_kernel() -> ! {
        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!(
                "ecall",
                in("a7") VSCHED2_INTO_KERNEL_SYSNO,
                options(noreturn),
            );
        }
        #[cfg(not(target_arch = "riscv64"))]
        unimplemented!("VschedContextImpl::into_kernel: unsupported architecture");
    }

    /// 内核态 → 用户态协程：设置用户栈并 sret 到 `raw_run_task`。
    fn into_user(ustack: usize) {
        let offset = RAW_RUN_TASK_OFFSET.load(Ordering::Acquire);
        assert_ne!(offset, 0, "into_user: raw_run_task offset not initialized");
        let current_task = libvsched2::current_task_ptr();
        let user_vdso_base = if !current_task.is_null() {
            let vti = unsafe { &*(current_task as *const VschedTaskImpl) };
            vti.user_vdso_base.load(Ordering::Acquire)
        } else {
            0
        };
        assert_ne!(
            user_vdso_base, 0,
            "into_user: user_vdso_base not set, did mm.rs call set_process_vdso_base?"
        );
        let entry = user_vdso_base + offset;

        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!(
                "csrw   sepc, {entry}",
                "li     t0, (1 << 5)",
                "csrs   sstatus, t0",     // SPIE=1 → sret enables interrupts
                "li     t0, (1 << 8)",
                "csrc   sstatus, t0",     // SPP=0 → sret goes to U-mode
                "mv     sp, {sp}",
                "sret",
                entry = in(reg) entry,
                sp = in(reg) ustack,
                options(noreturn),
            );
        }
        #[cfg(not(target_arch = "riscv64"))]
        unimplemented!("VschedContextImpl::into_user: unsupported architecture");
    }

    fn into_user_context(task: *const ()) {
        assert!(!task.is_null(), "into_user_context: task is null");
        let vsched_task = unsafe { &*(task as *const VschedTaskImpl) };
        let tf_ptr = vsched_task.trap_frame.load(Ordering::Acquire);
        assert_ne!(tf_ptr, 0, "into_user_context: trap_frame is null");
        let tf = unsafe { &*(tf_ptr as *const UserTrapFrame) };
        if tf.scause == 0 {
            axlog::info!(
                "[vsched2-diag] initial user restore task={:#x} axid={} name={} pid={} tf={:#x} satp={:#x} expected_root={:#x} vspace={:#x} vdso={:#x} sepc={:#x} sp={:#x} gp={:#x} tp={:#x} a0={:#x} sstatus={:#x}",
                task as usize,
                vsched_task.task.id().as_u64(),
                vsched_task.task.name(),
                vsched_task.pid.load(Ordering::Acquire),
                tf_ptr,
                axhal::asm::read_user_page_table().as_usize(),
                vsched_task.user_page_table_root.load(Ordering::Acquire),
                vsched_task.user_aspace_ptr.load(Ordering::Acquire),
                vsched_task.user_vdso_base.load(Ordering::Acquire),
                tf.sepc,
                tf.regs.sp,
                tf.regs.gp,
                tf.regs.tp,
                tf.regs.a0,
                tf.sstatus,
            );
        }
        let spp = (tf.sstatus >> 8) & 1;
        // axlog::ax_println!(
        assert_eq!(
            spp, 0,
            "into_user_context: frame would return to supervisor mode",
        );
        assert!(
            tf.sepc < 0xffffffc000000000,
            "into_user_context: user frame points into kernel: sepc={:#x}",
            tf.sepc,
        );
        // into_vspace already switched SATP to user PT. Load regs and sret.
        unsafe { tf.restore_and_sret() };
    }
}

impl libvsched2::VSpace for VschedVSpaceImpl {
    /// 切换到指定地址空间（写 SATP + 刷新 TLB + SUM）。
    /// vspace 通过 `from_mut(ptr)` 绑定到 `&self`。
    fn into_vspace(&self) {
        let vspace = self as *const Self as *mut ();
        if let Some(root) = page_table_root_from_raw(vspace) {
            let aspace = unsafe { &*(vspace as *const AddrSpace) };
            CURRENT_VDSO_BASE[<super::smp::VschedSmpImpl as libvsched2::SMP>::cpu_id()]
                .store(aspace.vdso_base, Ordering::Release);
            activate_user_aspace(root);
        }
    }

    fn dealloc(&self) {
        // The pointer registered in process_init is borrowed from the
        // ProcessData-owned Arc<Mutex<AddrSpace>>; vsched2 did not acquire a
        // separate strong reference.  process_drop therefore only releases
        // the scheduler slot, while ProcessData remains the sole owner.
    }
}
