//! VschedSmpImpl — vsched2 SMP 接口实现。

use axhal::percpu::this_cpu_id;

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
}
