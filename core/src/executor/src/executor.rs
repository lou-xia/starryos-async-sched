use alloc::{
    boxed::Box, string::{String, ToString}, sync::Arc, vec::{self, Vec}
};
use core::{
    ops::Deref,
    sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering},
};

use asynctask::{BaseScheduler, Task, TaskId, TaskInner, TaskRef, TrapFrame};
use axerrno::AxResult;
use axlog::debug;
use axfs::FS_CONTEXT;
use axmm::{AddrSpace, kernel_aspace};
use axsync::Mutex;
use kspin::SpinNoIrq;
use riscv::asm;
use scope_local::Scope;
use spin::RwLock;
use starry_core::{
    mm::{copy_from_kernel, load_user_app, new_user_aspace_empty},
    resources::Rlimits,
};
use starry_signal::api::{ProcessSignalManager, SignalActions};

use crate::{KERNEL_EXECUTOR, KERNEL_EXECUTOR_ID, KERNEL_SCHEDULER, UTRAP_HANDLER, table::PID2PC};
// use starry_process::Process;

pub struct Executor {
    pub pid: usize,
    pub parent: AtomicUsize,
    pub children: Mutex<Vec<Arc<Executor>>>,
    pub is_zombie: AtomicBool,
    pub exit_code: AtomicIsize,
    /// 地址空间，使用 axmm::AddrSpace 结构
    pub aspace: Option<Arc<Mutex<AddrSpace>>>,
    /// The resource scope 包括文件描述符
    pub scope: RwLock<Scope>,
    /// 堆底
    // 暂时不知道有没有用
    // pub heap_bottom: AtomicUsize,
    /// 堆顶
    pub heap_top: AtomicUsize,

    // vfork: 暂不考虑
    pub exe_path: RwLock<String>,
    pub cmdline: RwLock<Arc<Vec<String>>>,

    /// The resource limits
    pub rlim: RwLock<Rlimits>,

    /// The process signal manager
    pub signal: Arc<ProcessSignalManager>,

    pub main_task: Mutex<Option<TaskRef>>,
    pub tasks: Mutex<Vec<TaskRef>>,
}

impl Executor {
    pub fn new(
        pid: usize,
        parent: usize,
        aspace: Option<Arc<Mutex<AddrSpace>>>,
        exe_path: String,
        cmdline: Arc<Vec<String>>,
        signal_actions: Arc<SpinNoIrq<SignalActions>>,
    ) -> Self {
        Self {
            pid,
            parent: AtomicUsize::new(parent),
            children: Mutex::new(Vec::new()),
            is_zombie: AtomicBool::new(false),
            exit_code: AtomicIsize::new(0),
            aspace,
            scope: RwLock::new(Scope::new()),
            heap_top: AtomicUsize::new(starry_core::config::USER_HEAP_BASE),
            exe_path: RwLock::new(exe_path),
            cmdline: RwLock::new(cmdline),
            rlim: RwLock::default(),
            signal: Arc::new(ProcessSignalManager::new(
                signal_actions,
                starry_core::config::SIGNAL_TRAMPOLINE,
            )),
            main_task: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
        }
    }

    /// 初始化内核 Executor，地址空间为全局内核地址空间，还没有添加 stdio，需要在调用后添加，aspace 为 None，需要访问内核空间时使用 axmm::kernel_aspace()
    // myTODO：添加 stdio、aspace 改为指针而非 None
    pub fn new_init() -> Self {
        let executor = Self::new(
            KERNEL_EXECUTOR_ID,
            KERNEL_EXECUTOR_ID,
            None,
            "".into(),
            Arc::default(),
            Arc::default(),
        );
        {
            let mut scope = executor.scope.write();
            starry_api::file::add_stdio(
                &mut starry_api::file::FD_TABLE.scope_mut(&mut scope).write(),
            )
            .expect("Failed to add stdio");
        }
        executor
    }

    pub fn pid(&self) -> usize {
        self.pid
    }

    pub fn get_parent_pid(&self) -> usize {
        self.parent.load(Ordering::Acquire)
    }

    pub fn set_parent_pid(&self, parent: usize) {
        self.parent.store(parent, Ordering::Release)
    }

    /// 获取 Executor（进程）退出码
    pub fn get_exit_code(&self) -> isize {
        self.exit_code.load(Ordering::Acquire)
    }

    /// 设置 Executor（进程）退出码
    pub fn set_exit_code(&self, exit_code: isize) {
        self.exit_code.store(exit_code, Ordering::Release)
    }

    /// 判断 Executor（进程）是否处于僵尸状态
    pub fn get_zombie(&self) -> bool {
        self.is_zombie.load(Ordering::Acquire)
    }

    /// 设置 Executor（进程）是否处于僵尸状态
    pub fn set_zombie(&self, status: bool) {
        self.is_zombie.store(status, Ordering::Release)
    }

    pub fn set_heap_top(&self, top: usize) {
        self.heap_top.store(top, Ordering::Release)
    }

    pub fn get_heap_top(&self) -> usize {
        self.heap_top.load(Ordering::Acquire)
    }

    /// 设置 Executor（进程）可执行文件路径
    pub async fn set_exe_path(&self, path: String) {
        // let mut exe_path = self.exe_path.lock().await;
        let mut exe_path = self.exe_path.write();
        *exe_path = path;
    }

    /// 获取 Executor（进程）可执行文件路径
    pub async fn get_exe_path(&self) -> String {
        // (*self.exe_path.lock().await).clone();
        (*self.exe_path.read()).clone()
    }

    /// 若进程运行完成，则获取其返回码
    /// 若正在运行（可能上锁或没有上锁），则返回None
    pub fn get_code_if_exit(&self) -> Option<isize> {
        if self.get_zombie() {
            return Some(self.get_exit_code());
        }
        None
    }

    #[inline]
    /// Pick one task from Executor
    pub fn pick_next_task(&self) -> Option<TaskRef> {
        // self.scheduler.lock().pick_next_task()
        KERNEL_SCHEDULER.lock().pick_next_task()
    }

    pub async fn set_main_task(&self, task: TaskRef) {
        // *self.main_task.lock().await = Some(task);
        *self.main_task.lock() = Some(task);
    }

    pub async fn get_main_task(&self) -> Option<TaskRef> {
        // self.main_task.lock().await.clone()
        self.main_task.lock().clone()
    }

    pub async fn exit_main_task(&self) -> Option<TaskRef> {
        // self.main_task.lock().await.take()
        self.main_task.lock().take()
    }
}

impl Executor {
    pub async fn init_user(args: Vec<String>, envs: &Vec<String>) -> AxResult<TaskRef> {
        // let path = args.get(0).clone();
        let loc = FS_CONTEXT
            .lock()
            .resolve(&args[0])
            .expect("Failed to resolve executable path");
        let path = loc
            .absolute_path()
            .expect("Failed to get executable absolute path");
        let name = loc.name();
        drop(loc);

        let mut uspace = new_user_aspace_empty()
            .and_then(|mut it| {
                copy_from_kernel(&mut it)?;
                Ok(it)
            })
            .expect("Failed to create user address space");
        let page_table_root = uspace.page_table_root();
        axhal::asm::disable_irqs();
        if page_table_root.as_usize() != 0 {
            let page_table_token_pa = page_table_root.into();
            unsafe {
                // axhal::asm::write_page_table_root0(page_table_root.into());
                if page_table_token_pa != axhal::asm::read_user_page_table() {
                    axhal::asm::write_user_page_table(page_table_token_pa);
                    asm::sfence_vma_all();
                }
                #[cfg(target_arch = "riscv64")]
                riscv::register::sstatus::set_sum();
            };
        }
        let (entry_vaddr, ustack_top) =
            load_user_app(&mut uspace, Some(&path.to_string()), &args, envs)
                .unwrap_or_else(|e| panic!("Failed to load user app: {}", e));
        axhal::asm::enable_irqs();

        let mut executor = Arc::new(Self::new(
            TaskId::new().as_usize(),
            KERNEL_EXECUTOR_ID,
            Some(Arc::new(Mutex::new(uspace))),
            path.to_string(),
            Arc::new(args),
            Arc::default(),
        ));
        {
            let mut scope = executor.scope.write();
            starry_api::file::add_stdio(
                &mut starry_api::file::FD_TABLE.scope_mut(&mut scope).write(),
            )
            .expect("Failed to add stdio");
        }

        let scheduler = KERNEL_SCHEDULER.clone();
        let fut = UTRAP_HANDLER();
        let pid = executor.pid;
        let task = Arc::new(Task::new(TaskInner::new_user(
            path.to_string(),
            pid,
            scheduler,
            page_table_root.as_usize(),
            fut,
            Box::new(TrapFrame::init_user_context(
                entry_vaddr.into(),
                ustack_top.into(),
            )),
        )));
        executor.tasks.lock().push(task.clone());
        task.get_scheduler().lock().add_task(task.clone());
        task.set_leader(true);
        executor.set_main_task(task.clone());

        // myTODO：signal 和 robust_list 还没实现
        // new_executor
        //     .signal_modules
        //     .lock()
        //     .insert(new_task.id().as_u64(), SignalModule::init_signal(None));
        // new_executor
        //     .robust_list
        //     .lock()
        //     .insert(new_task.id().as_u64(), FutexRobustList::default());


        PID2PC
            .lock()
            .insert(executor.pid(), Arc::clone(&executor));
        // 记录内核 executor
        PID2PC
            .lock()
            .insert(KERNEL_EXECUTOR_ID, KERNEL_EXECUTOR.clone());
        // 将其作为内核进程的子进程
        KERNEL_EXECUTOR
            .children
            .lock()
            .push(executor.clone());
        Ok(task)
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        debug!("drop executor {}", self.pid);
    }
}
