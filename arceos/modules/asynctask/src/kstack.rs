use alloc::vec::Vec;
use core::{alloc::Layout, ptr::NonNull};
use lazyinit::LazyInit;
use memory_addr::VirtAddr;
use kspin::SpinNoIrq;

pub struct TaskStack {
    ptr: NonNull<u8>,
    layout: Layout,
    is_init: bool,
}

// arch_boot
unsafe extern "C" {
    fn current_boot_stack() -> *mut u8;
}

impl TaskStack {
    pub fn new_init() -> Self {
        let layout = Layout::from_size_align(axconfig::TASK_STACK_SIZE, 16).unwrap();
        unsafe {
            Self {
                ptr: NonNull::new(current_boot_stack()).unwrap(),
                layout,
                is_init: true,
            }
        }
    }

    pub fn alloc(size: usize) -> Self {
        let layout = Layout::from_size_align(size, 16).unwrap();
        Self {
            ptr: NonNull::new(unsafe { alloc::alloc::alloc(layout) }).unwrap(),
            layout,
            is_init: false,
        }
    }

    pub const fn top(&self) -> VirtAddr {
        unsafe { core::mem::transmute(self.ptr.as_ptr().add(self.layout.size())) }
    }

    pub const fn down(&self) -> VirtAddr {
        unsafe { core::mem::transmute(self.ptr.as_ptr()) }
    }
}

impl Drop for TaskStack {
    fn drop(&mut self) {
        if !self.is_init {
            unsafe { alloc::alloc::dealloc(self.ptr.as_ptr(), self.layout) }
        }
    }
}

#[percpu::def_percpu]
static STACK_POOL: LazyInit<SpinNoIrq<StackPool>> = LazyInit::new();

pub fn init() {
    STACK_POOL.with_current(|i| {
        let mut stack_pool = StackPool::new();
        stack_pool.init();
        i.init_once(SpinNoIrq::new(stack_pool));
    });
}

pub fn pick_current_stack() -> TaskStack {
    let mut stack_pool = unsafe { STACK_POOL.current_ref_mut_raw().lock() };
    stack_pool.pick_current_stack()
}

pub fn current_stack_top() -> usize {
    let stack_pool = unsafe { STACK_POOL.current_ref_mut_raw().lock() };
    stack_pool.current_stack().top().as_usize()
}

pub fn put_prev_stack(kstack: TaskStack) {
    let mut stack_pool = unsafe { STACK_POOL.current_ref_mut_raw().lock() };
    stack_pool.put_prev_stack(kstack)
}

/// A simple stack pool
pub(crate) struct StackPool {
    free_stacks: Vec<TaskStack>,
    current: Option<TaskStack>,
}

impl StackPool {
    /// Creates a new empty stack pool.
    pub const fn new() -> Self {
        Self {
            free_stacks: Vec::new(),
            current: None,
        }
    }

    pub fn init(&mut self) {
        self.current = Some(TaskStack::new_init());
    }

    /// Alloc a free stack from the pool.
    fn alloc(&mut self) -> TaskStack {
        self.free_stacks.pop().unwrap_or_else(|| {
            let stack = TaskStack::alloc(axconfig::TASK_STACK_SIZE);
            stack
        })
    }

    pub fn pick_current_stack(&mut self) -> TaskStack {
        assert!(self.current.is_some());
        let new_stack = self.alloc();
        self.current.replace(new_stack).unwrap()
    }

    pub fn current_stack(&self) -> &TaskStack {
        assert!(self.current.is_some());
        self.current.as_ref().unwrap()
    }

    pub fn put_prev_stack(&mut self, kstack: TaskStack) {
        assert!(self.current.is_some());
        let curr_stack = self.current.replace(kstack).unwrap();
        self.free_stacks.push(curr_stack);
    }
}
