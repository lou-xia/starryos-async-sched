use core::{mem::ManuallyDrop, ops::Deref, task::Waker};

use alloc::sync::Arc;

use crate::TaskRef;

pub struct CurrentTask(ManuallyDrop<TaskRef>);

impl CurrentTask {
    pub fn try_get() -> Option<Self> {
        let ptr: *const super::Task = axhal::percpu::current_task_ptr();
        if !ptr.is_null() {
            Some(Self(unsafe { ManuallyDrop::new(TaskRef::from_raw(ptr)) }))
        } else {
            None
        }
    }

    pub fn get() -> Self {
        Self::try_get().expect("current task is uninitialized")
    }
    
    /// Converts [`CurrentTask`] to [`TaskRef`].
    pub fn as_task_ref(&self) -> &TaskRef {
        &self.0
    }

    /// Clone the inner `AxTaskRef`.
    #[allow(clippy::should_implement_trait)]
    pub fn clone(&self) -> TaskRef {
        self.0.deref().clone()
    }

    /// Returns `true` if the current task is the same as `other`.
    pub fn ptr_eq(&self, other: &TaskRef) -> bool {
        Arc::ptr_eq(&self.0, other)
    }

    pub unsafe fn init_current(init_task: TaskRef) {
        assert!(init_task.is_init());
        // #[cfg(feature = "tls")]  myTODO: TLS
        // unsafe {
        //     axhal::asm::write_thread_pointer(init_task.tls.tls_ptr() as usize)
        // };
        init_task.set_state(crate::TaskState::Running);
        let ptr = Arc::into_raw(init_task);
        unsafe {
            axhal::percpu::set_current_task_ptr(ptr);
        }
    }

    pub unsafe fn set_current(prev: Self, next: TaskRef) {
        let Self(arc) = prev;
        ManuallyDrop::into_inner(arc); // `call Arc::drop()` to decrease prev task reference count.
        let ptr = Arc::into_raw(next);
        unsafe {
            axhal::percpu::set_current_task_ptr(ptr);
        }
    }

    pub fn clean_current() {
        let curr = Self::get();
        let Self(arc) = curr;
        ManuallyDrop::into_inner(arc); // `call Arc::drop()` to decrease prev task reference count.
        unsafe { axhal::percpu::set_current_task_ptr(0 as *const crate::Task) };
    }

    pub fn clean_current_without_drop() -> *const super::Task {
        let ptr: *const super::Task = axhal::percpu::current_task_ptr();
        unsafe { axhal::percpu::set_current_task_ptr(0 as *const crate::Task) };
        ptr
    }

    pub fn waker(&self) -> Waker {
        crate::waker::waker_from_task(axhal::percpu::current_task_ptr() as _)
    }
}

impl Deref for CurrentTask {
    type Target = TaskRef;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}