use core::{
    cell::UnsafeCell,
    mem::{ManuallyDrop, MaybeUninit},
    ops::Deref,
    sync::atomic::{AtomicU8, Ordering},
};

use crate::{
    ARRAY_LEN, PerProcess, QUEUE_CAPACITY, deque::LockFreeDeque, get_queue_array, ipc_item::IPCItem,
};

pub struct SlotArray<T, const N: usize> {
    slots: [Slot<T>; N],
}

const SLOT_EMPTY: u8 = 0;
const SLOT_READY: u8 = 1;
const SLOT_PENDING: u8 = 2;

struct Slot<T> {
    state: AtomicU8,
    rc: AtomicU8,
    value: UnsafeCell<MaybeUninit<T>>,
}

// low-level operations
impl<T, const N: usize> SlotArray<T, N> {
    /// Attempts to push a value into the slot array.
    /// Returns the index of the slot if successful, or an error if the array is full.
    fn push_(&self, value: T) -> Result<usize, ()> {
        for i in 0..N {
            let Slot {
                state,
                rc,
                value: prev_value,
            } = &self.slots[i];
            if let Ok(prev) = state.compare_exchange(
                SLOT_EMPTY,
                SLOT_PENDING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                assert_eq!(prev, SLOT_EMPTY);
                // Safe using `get` because we have exclusive access to this slot by setting state to SLOT_PENDING
                // Safe using `write` because we are initializing the slot
                unsafe {
                    (&mut *prev_value.get()).write(value);
                }
                let prev = state.swap(SLOT_READY, Ordering::AcqRel);
                assert_eq!(prev, SLOT_PENDING);
                let prev_rc = rc.fetch_add(1, Ordering::AcqRel);
                assert_eq!(prev_rc, 0);
                return Ok(i);
            }
        }
        Err(())
    }

    pub(crate) fn get(&self, index: usize) -> Option<&T> {
        let Slot {
            state,
            rc: _,
            value,
        } = &self.slots[index];
        if state.load(Ordering::Acquire) == SLOT_READY {
            let res = Some(unsafe { (&*value.get()).assume_init_ref() });
            if state.load(Ordering::Acquire) == SLOT_READY {
                res
            } else {
                // state changed, return None
                None
            }
        } else {
            None
        }
    }

    /// Deletes a value from the slot array at the given index.
    ///
    /// # Safety
    ///
    /// The caller must ensure:
    ///
    /// - the index is valid
    /// - the slot at that index is initialized
    /// - the state at that index is currently in the `SLOT_PENDING` state.
    /// - the caller has exclusive access to the slot (`rc == 0` because `rc` is already decreased in `SlotRef::drop`).
    unsafe fn delete(&self, index: usize) {
        let Slot { state, rc, value } = &self.slots[index];
        let prev = state.swap(SLOT_EMPTY, Ordering::AcqRel);
        assert_eq!(prev, SLOT_PENDING);
        // Safe because we have exclusive access to this slot by setting state to SLOT_PENDING
        unsafe {
            (&mut *value.get()).assume_init_drop();
        }
        let rc = rc.load(Ordering::Acquire);
        assert_eq!(rc, 0);
    }

    /// 释放一个引用计数恰好为1的槽位
    ///
    /// 仅用于特定用途
    pub(crate) unsafe fn drop_slot(&self, index: usize) {
        let Slot { state, rc, .. } = &self.slots[index];
        let prev_rc = rc.fetch_sub(1, Ordering::AcqRel);
        assert!(prev_rc == 1);

        let prev_state = state.swap(SLOT_PENDING, Ordering::Release);
        assert_eq!(prev_state, SLOT_READY);

        unsafe {
            self.delete(index);
        }
    }
}

impl<T, const N: usize> Default for SlotArray<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl<T, const N: usize> Sync for SlotArray<T, N> where T: Sync {}
unsafe impl<T, const N: usize> Send for SlotArray<T, N> where T: Send {}

/// 对应于`SlotArray`中一个槽位的引用，包含了槽位所在的数组和槽位的索引。
///
/// 通过引用计数管理槽位的释放。
pub struct SlotRef<'a, T, const N: usize> {
    array: &'a SlotArray<T, N>,
    pub(crate) index: usize,
}

impl<'a, T, const N: usize> core::fmt::Debug for SlotRef<'a, T, N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SlotRef")
            .field("array", &(self.array as *const SlotArray<T, N>))
            .field("index", &self.index)
            .finish()
    }
}

impl<'a, T, const N: usize> SlotRef<'a, T, N> {
    /// 调试用接口
    pub fn rc(&self) -> u8 {
        self.array.slots[self.index].rc.load(Ordering::Acquire)
    }
}

/// Conversions between `SlotRef` and usize IDs
///
/// When converting to an ID, the `SlotRef` will not be dropped
/// until the ID is converted back to a `SlotRef`.
/// (Similar to `Arc::into_raw` and `Arc::from_raw`)
impl SlotRef<'static, PerProcess, ARRAY_LEN> {
    /// Converts a `SlotRef` into a usize ID.
    pub fn into_id(self) -> usize {
        let id = self.index;
        core::mem::forget(self);
        // let _ = ManuallyDrop::new(self);
        id
    }

    /// 使用了`get_queue_array`的函数，只能通过API暴露给外界。
    ///
    /// # Safety
    ///
    /// The caller must ensure that the id is get from `SlotRef::into_id`.
    ///
    /// one id can only be converted back to one `SlotRef`.
    pub(crate) unsafe fn from_id(id: usize) -> Self {
        assert!(id < ARRAY_LEN, "SlotRef::from_id: id out of bounds");
        let array = get_queue_array();
        let Slot {
            state,
            rc,
            value: _,
        } = &array.slots[id];
        assert_eq!(state.load(Ordering::Acquire), SLOT_READY);
        assert!(rc.load(Ordering::Acquire) >= 1);
        Self { array, index: id }
    }

    // pub fn id(&self) -> usize {
    //     self.index
    // }

    // /// error code:
    // /// - 1: id out of bounds
    // /// - 2: slot not ready
    // pub fn try_from_id(id: usize) -> Result<Self, usize> {
    //     if id >= ARRAY_LEN {
    //         return Err(1); // id out of bounds
    //     }
    //     let array = get_queue_array();
    //     let Slot { state, rc, value } = &array.slots[id];
    //     if state
    //         .compare_exchange(
    //             SLOT_READY,
    //             SLOT_PENDING,
    //             Ordering::AcqRel,
    //             Ordering::Acquire,
    //         )
    //         .is_err()
    //     {
    //         return Err(2); // slot not ready
    //     }
    //     rc.fetch_add(1, Ordering::AcqRel);
    //     // with the above fetch_add, rc must be >= 1.
    //     // so we can restore the state to SLOT_READY and return the SlotRef safely.
    //     let old_state = state.swap(SLOT_READY, Ordering::AcqRel);
    //     assert_eq!(old_state, SLOT_PENDING);
    //     Ok(Self { array, index: id })
    // }
}

unsafe impl<T: Sync, const N: usize> Send for SlotRef<'_, T, N> {}

// -------- high-level operations --------

impl<T, const N: usize> SlotArray<T, N> {
    pub const fn new() -> Self {
        Self {
            slots: [const {
                Slot {
                    state: AtomicU8::new(SLOT_EMPTY),
                    rc: AtomicU8::new(0),
                    value: UnsafeCell::new(MaybeUninit::uninit()),
                }
            }; N],
        }
    }
}

impl<'a, T, const N: usize> SlotArray<T, N> {
    /// Pushes a value into the slot array and returns a `SlotRef` to it.
    pub fn push(&'a self, value: T) -> Result<SlotRef<'a, T, N>, ()> {
        let index = self.push_(value)?;
        Ok(SlotRef { array: self, index })
    }
}

impl<'a, T, const N: usize> SlotRef<'a, T, N> {
    /// get a reference to a slot in the array
    /// safe because the SlotRef guarantees that the slot is valid
    pub fn get(&self) -> &'a T {
        self.array.get(self.index).unwrap()
    }
}

impl<'a, T, const N: usize> Deref for SlotRef<'a, T, N> {
    type Target = T;

    fn deref(&self) -> &'a Self::Target {
        self.get()
    }
}

impl<'a, T, const N: usize> Clone for SlotRef<'a, T, N> {
    fn clone(&self) -> Self {
        let prev_rc = self.array.slots[self.index]
            .rc
            .fetch_add(1, Ordering::AcqRel);
        assert!(prev_rc >= 1);
        Self {
            array: self.array,
            index: self.index,
        }
    }
}

impl<'a, T, const N: usize> Drop for SlotRef<'a, T, N> {
    fn drop(&mut self) {
        let prev_rc = self.array.slots[self.index]
            .rc
            .fetch_sub(1, Ordering::AcqRel);
        if prev_rc == 1 {
            let prev_state = self.array.slots[self.index]
                .state
                .swap(SLOT_PENDING, Ordering::Release);
            assert_eq!(prev_state, SLOT_READY);
            // Safe because the caller has exclusive access to the slot
            unsafe {
                self.array.delete(self.index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::{SlotArray, SlotRef};

    #[test]
    fn test_sequential() {
        let array: SlotArray<usize, 4> = SlotArray::new();
        let slot1 = array.push(10).unwrap();
        let slot2 = array.push(20).unwrap();
        let slot3 = array.push(30).unwrap();
        let slot4 = array.push(40).unwrap();
        let should_err = array.push(50);
        assert_eq!(*slot1, 10);
        assert_eq!(*slot2, 20);
        assert_eq!(*slot3, 30);
        assert_eq!(*slot4, 40);
        assert!(should_err.is_err());

        let slot1_clone = slot1.clone();
        assert_eq!(*slot1_clone, 10);

        drop(slot1);
        assert_eq!(*slot1_clone, 10);

        drop(slot1_clone);
        // At this point, the slot for 10 should be deleted.

        let slot6 = array.push(60).unwrap();
        assert_eq!(*slot6, 60);
    }

    const THREAD_NUM: usize = 16;
    const DATA_PER_THREAD: usize = 1000;
    const TOTAL_DATA: usize = (THREAD_NUM + 1) * DATA_PER_THREAD;
    static ARRAY: SlotArray<std::sync::atomic::AtomicUsize, TOTAL_DATA> = SlotArray::new();
    #[test]
    fn test_parallel() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;
        use std::thread::*;
        use std::vec::*;

        // let array: Arc<SlotArray<AtomicUsize, TOTAL_DATA>> = Arc::new(SlotArray::new());
        let mut handles: Vec<JoinHandle<()>> = Vec::new();
        let mut slots: Arc<Vec<SlotRef<'_, AtomicUsize, TOTAL_DATA>>> = Arc::new(Vec::new());
        for i in 0..DATA_PER_THREAD {
            let value = AtomicUsize::new(i);
            let slot = ARRAY.push(value).unwrap();
            Arc::get_mut(&mut slots).unwrap().push(slot);
        }
        for t in 0..THREAD_NUM {
            let slots_clone = slots.clone();
            let handle = spawn(move || {
                // testing push in parallel
                let mut local_slots: Vec<SlotRef<'_, AtomicUsize, TOTAL_DATA>> = Vec::new();
                for i in 0..DATA_PER_THREAD {
                    let value = AtomicUsize::new((t + 1) * DATA_PER_THREAD + i);
                    let slot = ARRAY.push(value).unwrap();
                    assert_eq!(
                        slot.get().load(Ordering::Acquire),
                        (t + 1) * DATA_PER_THREAD + i
                    );
                    local_slots.push(slot);
                }

                yield_now();

                // testing shared access in parallel
                let mut cloned_slots: Vec<SlotRef<'_, AtomicUsize, TOTAL_DATA>> = Vec::new();
                for slot in slots_clone.iter() {
                    let slot_clone = slot.clone();
                    slot_clone.get().fetch_add(1, Ordering::AcqRel);
                    cloned_slots.push(slot_clone);
                }

                yield_now();

                // testing drop in parallel
                drop(local_slots);
                drop(cloned_slots);
            });
            handles.push(handle);
        }
        for handle in handles {
            handle.join().unwrap();
        }
        // verify the results
        for i in 0..DATA_PER_THREAD {
            assert_eq!(slots[i].get().load(Ordering::Acquire), i + THREAD_NUM);
        }
    }
}
