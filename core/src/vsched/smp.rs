use axhal::percpu::this_cpu_id;

pub struct VschedSmpImpl;

impl libvsched2::SMP for VschedSmpImpl {
    fn cpu_id() -> usize {
        this_cpu_id()
    }
}
