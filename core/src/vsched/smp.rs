//! VschedSmpImpl — vsched2 SMP 接口实现。

use axhal::{
    irq::{IPI_IRQ, IpiTarget},
    percpu::this_cpu_id,
};

pub struct VschedSmpImpl;

impl libvsched2::SMP for VschedSmpImpl {
    fn cpu_id() -> usize {
        let cpu_id = this_cpu_id();
        assert!(
            cpu_id < axconfig::plat::CPU_NUM,
            "vsched2 cpu_id {} exceeds StarryOS CPU_NUM {}",
            cpu_id,
            axconfig::plat::CPU_NUM,
        );
        cpu_id
    }

    fn send_ipi(target_cpu: usize) {
        let current_cpu = this_cpu_id();
        assert!(
            target_cpu < axconfig::plat::CPU_NUM,
            "vsched2 IPI target {} exceeds StarryOS CPU_NUM {}",
            target_cpu,
            axconfig::plat::CPU_NUM,
        );
        assert_ne!(
            target_cpu, current_cpu,
            "vsched2 must not send a scheduler wake IPI to the current CPU",
        );

        // vsched2 只需要中断目标核心的 WFI，不需要携带 axipi 回调。
        axhal::irq::send_ipi(IPI_IRQ, IpiTarget::Other { cpu_id: target_cpu });
    }
}
