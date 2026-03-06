use core::{mem::ManuallyDrop, ops::Deref};

use alloc::sync::Arc;
use axhal::percpu::{current_executor_ptr, set_current_executor_ptr};

use crate::{ExecutorRef, executor::Executor};

pub struct CurrentExecutor(ManuallyDrop<ExecutorRef>);

impl CurrentExecutor {
    pub(crate) fn try_get() -> Option<Self> {
        let ptr: *const Executor = current_executor_ptr();
        if !ptr.is_null() {
            Some(Self(unsafe {
                ManuallyDrop::new(ExecutorRef::from_raw(ptr))
            }))
        } else {
            None
        }
    }

    pub fn get() -> Self {
        Self::try_get().expect("current executor is uninitialized")
    }

    #[allow(unused)]
    /// Converts [`CurrentTask`] to [`TaskRef`].
    pub fn as_executor_ref(&self) -> &ExecutorRef {
        &self.0
    }

    #[allow(unused)]
    pub fn clone(&self) -> ExecutorRef {
        self.0.deref().clone()
    }

    #[allow(unused)]
    pub fn ptr_eq(&self, other: &ExecutorRef) -> bool {
        Arc::ptr_eq(&self.0, other)
    }

    pub unsafe fn init_current(executor: ExecutorRef) {
        let ptr = Arc::into_raw(executor);
        unsafe { set_current_executor_ptr(ptr) };
    }

    pub fn clean_current() {
        let curr = Self::get();
        let Self(arc) = curr;
        ManuallyDrop::into_inner(arc); // `call Arc::drop()` to decrease prev task reference count.
        unsafe { set_current_executor_ptr(0 as *const Executor) };
    }

    pub fn clean_current_without_drop() {
        unsafe { set_current_executor_ptr(0 as *const Executor) };
    }
}

impl Deref for CurrentExecutor {
    type Target = Executor;
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}
