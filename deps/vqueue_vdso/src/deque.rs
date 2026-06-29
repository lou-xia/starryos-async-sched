//! 用于实现IPC队列的无锁双端队列（deque）数据结构，支持多生产者多消费者（MPMC）场景。
//!
//! Satety:
//!     Work when the queue is full in the MPMC situation will cause error.
//!
//! Copied and modified from [https://github.com/AsyncModules/vsched/blob/main/utils/src/deque.rs](https://github.com/AsyncModules/vsched/blob/main/utils/src/deque.rs).

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

// Slot states for tracking initialization
const SLOT_EMPTY: u8 = 0;
const SLOT_WRITING: u8 = 1;
const SLOT_READY: u8 = 2;
const SLOT_READING: u8 = 3;

struct Slot<T> {
    data: UnsafeCell<MaybeUninit<T>>,
    state: AtomicU8,
}

impl<T> Slot<T> {
    const fn new() -> Self {
        Self {
            data: UnsafeCell::new(MaybeUninit::uninit()),
            state: AtomicU8::new(SLOT_EMPTY),
        }
    }
}

/// A guard that holds a slot for writing. The slot will be marked as ready when the guard is dropped.
pub struct SlotGuard<'a, T> {
    slot: &'a Slot<T>,
}

impl<'a, T> Deref for SlotGuard<'a, T> {
    type Target = MaybeUninit<T>;

    fn deref(&self) -> &Self::Target {
        // Safe because the slot is guaranteed to be in WRITING state
        unsafe { &*self.slot.data.get() }
    }
}

impl<'a, T> DerefMut for SlotGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Safe because the slot is guaranteed to be in WRITING state
        unsafe { &mut *self.slot.data.get() }
    }
}

impl<'a, T> Drop for SlotGuard<'a, T> {
    fn drop(&mut self) {
        // Mark the slot as ready after writing
        self.slot.state.store(SLOT_READY, Ordering::Release);
    }
}

/// A lock-free deque implementation with fixed capacity, supporting multiple producers and multiple consumers.
pub struct LockFreeDeque<T, const CAPACITY: usize> {
    buffer: [Slot<T>; CAPACITY],
    head: AtomicUsize, // Points to the first element
    tail: AtomicUsize, // Points to one past the last element
}

impl<T, const CAPACITY: usize> LockFreeDeque<T, CAPACITY> {
    const EMPTY_CELL: Slot<T> = Slot::new();

    /// Create a new lock-free deque with compile-time capacity
    pub const fn new() -> Self {
        let buffer = [Self::EMPTY_CELL; CAPACITY];

        Self {
            buffer,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Push an item to the front of the deque
    /// Returns Err(item) if the deque is full
    pub fn push_front(&self, item: T) -> Result<(), T> {
        loop {
            let head = self.head.load(Ordering::Acquire);
            let tail = self.tail.load(Ordering::Acquire);
            let head_ = self.head.load(Ordering::Acquire);
            if head_ != head {
                continue;
            }

            // Calculate the new head position (moving backwards)
            let new_head = if head == 0 { CAPACITY - 1 } else { head - 1 };

            // Check if queue is full
            if new_head == tail {
                return Err(item);
            }

            // Check if the target slot is available
            let slot = &self.buffer[new_head];

            // Try to claim the slot for writing atomically
            match slot.state.compare_exchange_weak(
                SLOT_EMPTY,
                SLOT_WRITING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // Successfully claimed slot, now try to update head
                    match self.head.compare_exchange_weak(
                        head,
                        new_head,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            // Successfully reserved the slot, write the item
                            unsafe {
                                (*slot.data.get()).write(item);
                            }

                            // Mark slot as ready
                            slot.state.store(SLOT_READY, Ordering::Release);
                            return Ok(());
                        }
                        Err(_) => {
                            // Failed to update head, release the slot and retry
                            slot.state.store(SLOT_EMPTY, Ordering::Release);
                            // Small backoff to reduce contention
                            for _ in 0..5 {
                                core::hint::spin_loop();
                            }
                            continue;
                        }
                    }
                }
                Err(current_state) => {
                    // Slot is not empty
                    if current_state == SLOT_WRITING || current_state == SLOT_READING {
                        // Another thread is writing or reading, wait a bit
                        for _ in 0..10 {
                            core::hint::spin_loop();
                        }
                    }
                    continue;
                }
            }
        }
    }

    /// Push an item to the back of the deque
    /// Returns Err(item) if the deque is full
    pub fn push_back(&self, item: T) -> Result<(), T> {
        loop {
            let tail = self.tail.load(Ordering::Acquire);
            let head = self.head.load(Ordering::Acquire);
            let tail_ = self.tail.load(Ordering::Acquire);
            if tail_ != tail {
                continue;
            }

            // Calculate the new tail position
            let new_tail = (tail + 1) % CAPACITY;

            // Check if queue is full
            if new_tail == head {
                return Err(item);
            }

            // Check if the target slot is available
            let slot = &self.buffer[tail];

            // Try to claim the slot for writing atomically
            match slot.state.compare_exchange_weak(
                SLOT_EMPTY,
                SLOT_WRITING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // Successfully claimed slot, now try to update tail
                    match self.tail.compare_exchange_weak(
                        tail,
                        new_tail,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            // Successfully reserved the slot, write the item
                            unsafe {
                                (*slot.data.get()).write(item);
                            }

                            // Mark slot as ready
                            slot.state.store(SLOT_READY, Ordering::Release);
                            return Ok(());
                        }
                        Err(_) => {
                            // Failed to update tail, release the slot and retry
                            slot.state.store(SLOT_EMPTY, Ordering::Release);
                            // Small backoff to reduce contention
                            for _ in 0..5 {
                                core::hint::spin_loop();
                            }
                            continue;
                        }
                    }
                }
                Err(current_state) => {
                    // Slot is not empty
                    if current_state == SLOT_WRITING || current_state == SLOT_READING {
                        // Another thread is writing or reading, wait a bit
                        for _ in 0..10 {
                            core::hint::spin_loop();
                        }
                    }
                    continue;
                }
            }
        }
    }

    /// Push a slot to the front of the deque, returning a guard to the slot for in-place construction
    /// Drops the guard to finalize the slot
    ///
    /// Returns Err(item) if the deque is full
    pub fn push_slot_front(&self) -> Result<SlotGuard<'_, T>, ()> {
        loop {
            let head = self.head.load(Ordering::Acquire);
            let tail = self.tail.load(Ordering::Acquire);
            let head_ = self.head.load(Ordering::Acquire);
            if head_ != head {
                continue;
            }

            // Calculate the new head position (moving backwards)
            let new_head = if head == 0 { CAPACITY - 1 } else { head - 1 };

            // Check if queue is full
            if new_head == tail {
                return Err(());
            }

            // Check if the target slot is available
            let slot = &self.buffer[new_head];

            // Try to claim the slot for writing atomically
            match slot.state.compare_exchange_weak(
                SLOT_EMPTY,
                SLOT_WRITING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // Successfully claimed slot, now try to update head
                    match self.head.compare_exchange_weak(
                        head,
                        new_head,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            return Ok(SlotGuard { slot });
                        }
                        Err(_) => {
                            // Failed to update head, release the slot and retry
                            slot.state.store(SLOT_EMPTY, Ordering::Release);
                            // Small backoff to reduce contention
                            for _ in 0..5 {
                                core::hint::spin_loop();
                            }
                            continue;
                        }
                    }
                }
                Err(current_state) => {
                    // Slot is not empty
                    if current_state == SLOT_WRITING || current_state == SLOT_READING {
                        // Another thread is writing or reading, wait a bit
                        for _ in 0..10 {
                            core::hint::spin_loop();
                        }
                    }
                    continue;
                }
            }
        }
    }

    /// Push a slot to the back of the deque, returning a guard to the slot for in-place construction
    /// Drops the guard to finalize the slot
    ///
    /// Returns Err(item) if the deque is full
    pub fn push_slot_back(&self) -> Result<SlotGuard<'_, T>, ()> {
        loop {
            let tail = self.tail.load(Ordering::Acquire);
            let head = self.head.load(Ordering::Acquire);
            let tail_ = self.tail.load(Ordering::Acquire);
            if tail_ != tail {
                continue;
            }

            // Calculate the new tail position
            let new_tail = (tail + 1) % CAPACITY;

            // Check if queue is full
            if new_tail == head {
                return Err(());
            }

            // Check if the target slot is available
            let slot = &self.buffer[tail];

            // Try to claim the slot for writing atomically
            match slot.state.compare_exchange_weak(
                SLOT_EMPTY,
                SLOT_WRITING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // Successfully claimed slot, now try to update tail
                    match self.tail.compare_exchange_weak(
                        tail,
                        new_tail,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            return Ok(SlotGuard { slot });
                        }
                        Err(_) => {
                            // Failed to update tail, release the slot and retry
                            slot.state.store(SLOT_EMPTY, Ordering::Release);
                            // Small backoff to reduce contention
                            for _ in 0..5 {
                                core::hint::spin_loop();
                            }
                            continue;
                        }
                    }
                }
                Err(current_state) => {
                    // Slot is not empty
                    if current_state == SLOT_WRITING || current_state == SLOT_READING {
                        // Another thread is writing or reading, wait a bit
                        for _ in 0..10 {
                            core::hint::spin_loop();
                        }
                    }
                    continue;
                }
            }
        }
    }

    /// Pop an item from the front of the deque
    /// Returns None if the deque is empty
    pub fn pop_front(&self) -> Option<T> {
        loop {
            let head = self.head.load(Ordering::Acquire);
            let tail = self.tail.load(Ordering::Acquire);
            let head_ = self.head.load(Ordering::Acquire);
            if head_ != head {
                continue;
            }

            // Check if queue is empty
            if head == tail {
                return None;
            }

            // Check if the slot has data ready
            let slot = &self.buffer[head];

            // Try to claim the slot for reading
            match slot.state.compare_exchange_weak(
                SLOT_READY,
                SLOT_READING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // Successfully claimed slot for reading
                    let new_head = (head + 1) % CAPACITY;

                    // Try to update head
                    match self.head.compare_exchange_weak(
                        head,
                        new_head,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            // Successfully updated head, read the item
                            let item = unsafe { (*slot.data.get()).assume_init_read() };

                            // Mark slot as empty
                            slot.state.store(SLOT_EMPTY, Ordering::Release);
                            return Some(item);
                        }
                        Err(_) => {
                            // Failed to update head, restore slot state and retry
                            slot.state.store(SLOT_READY, Ordering::Release);
                            // Small backoff to reduce contention
                            for _ in 0..5 {
                                core::hint::spin_loop();
                            }
                            continue;
                        }
                    }
                }
                Err(current_state) => {
                    if current_state == SLOT_WRITING || current_state == SLOT_READING {
                        // Another thread is writing or reading, wait a bit
                        for _ in 0..10 {
                            core::hint::spin_loop();
                        }
                    }
                    continue;
                }
            }
        }
    }

    /// Pop an item from the back of the deque
    /// Returns None if the deque is empty
    pub fn pop_back(&self) -> Option<T> {
        loop {
            let tail = self.tail.load(Ordering::Acquire);
            let head = self.head.load(Ordering::Acquire);
            let tail_ = self.tail.load(Ordering::Acquire);
            if tail_ != tail {
                continue;
            }

            // Check if queue is empty
            if head == tail {
                return None;
            }

            // Calculate the position of the last element
            let last_pos = if tail == 0 { CAPACITY - 1 } else { tail - 1 };

            // Check if the slot has data ready
            let slot = &self.buffer[last_pos];

            // Try to claim the slot for reading
            match slot.state.compare_exchange_weak(
                SLOT_READY,
                SLOT_READING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // Successfully claimed slot for reading

                    // Try to update tail
                    match self.tail.compare_exchange_weak(
                        tail,
                        last_pos,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            // Successfully updated tail, read the item
                            let item = unsafe { (*slot.data.get()).assume_init_read() };

                            // Mark slot as empty
                            slot.state.store(SLOT_EMPTY, Ordering::Release);
                            return Some(item);
                        }
                        Err(_) => {
                            // Failed to update tail, restore slot state and retry
                            slot.state.store(SLOT_READY, Ordering::Release);
                            // Small backoff to reduce contention
                            for _ in 0..5 {
                                core::hint::spin_loop();
                            }
                            continue;
                        }
                    }
                }
                Err(current_state) => {
                    if current_state == SLOT_WRITING || current_state == SLOT_READING {
                        // Another thread is writing or reading, wait a bit
                        for _ in 0..10 {
                            core::hint::spin_loop();
                        }
                    }
                    continue;
                }
            }
        }
    }

    /// Get the current length of the deque (approximate in concurrent scenarios)
    pub fn len(&self) -> usize {
        let (head, tail) = loop {
            let head = self.head.load(Ordering::Acquire);
            let tail = self.tail.load(Ordering::Acquire);
            let head_ = self.head.load(Ordering::Acquire);
            if head_ == head {
                break (head, tail);
            }
        };

        if tail >= head {
            tail - head
        } else {
            CAPACITY - head + tail
        }
    }

    /// Check if the deque is empty (approximate in concurrent scenarios)
    pub fn is_empty(&self) -> bool {
        let (head, tail) = loop {
            let head = self.head.load(Ordering::Acquire);
            let tail = self.tail.load(Ordering::Acquire);
            let head_ = self.head.load(Ordering::Acquire);
            if head_ == head {
                break (head, tail);
            }
        };
        head == tail
    }

    /// Get the capacity of the deque
    pub const fn capacity(&self) -> usize {
        CAPACITY
    }
}

impl<T, const CAPACITY: usize> Default for LockFreeDeque<T, CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const CAPACITY: usize> Drop for LockFreeDeque<T, CAPACITY> {
    fn drop(&mut self) {
        // Clean up any remaining items to prevent memory leaks
        while self.pop_front().is_some() {}
    }
}

// Safety: The deque can be sent between threads if T can be sent
unsafe impl<T: Send, const CAPACITY: usize> Send for LockFreeDeque<T, CAPACITY> {}
// Safety: The deque can be shared between threads if T can be sent
unsafe impl<T: Send, const CAPACITY: usize> Sync for LockFreeDeque<T, CAPACITY> {}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use core::sync::atomic::AtomicI32;
    use std::{println, sync::Arc, thread, vec};
    #[test]
    fn test_basic_operations() {
        let deque: LockFreeDeque<i32, 5> = LockFreeDeque::new();

        // Test push_back and pop_front
        assert!(deque.push_back(1).is_ok());
        assert!(deque.push_back(2).is_ok());
        assert_eq!(deque.pop_front(), Some(1));
        assert_eq!(deque.pop_front(), Some(2));
        assert_eq!(deque.pop_front(), None);

        // Test push_front and pop_back
        assert!(deque.push_front(3).is_ok());
        assert!(deque.push_front(4).is_ok());
        assert_eq!(deque.pop_back(), Some(3));
        assert_eq!(deque.pop_back(), Some(4));
        assert_eq!(deque.pop_back(), None);
    }

    #[test]
    fn test_capacity_limit() {
        let deque: LockFreeDeque<i32, 3> = LockFreeDeque::new();

        assert!(deque.push_back(1).is_ok());
        assert!(deque.push_back(2).is_ok());
        assert!(deque.push_back(3).is_err()); // Should fail, queue is full
    }

    #[test]
    fn test_concurrent_operations() {
        let deque = Arc::new(LockFreeDeque::<i32, 100>::new());
        let mut handles = vec![];

        // Spawn multiple producers
        for i in 0..4 {
            let deque_clone = Arc::clone(&deque);
            let handle = thread::spawn(move || {
                for j in 0..25 {
                    let value = i * 25 + j;
                    while deque_clone.push_back(value).is_err() {
                        thread::yield_now();
                    }
                }
            });
            handles.push(handle);
        }

        // Spawn multiple consumers
        for _ in 0..2 {
            let deque_clone = Arc::clone(&deque);
            let handle = thread::spawn(move || {
                let mut count = 0;
                while count < 50 {
                    if let Some(_) = deque_clone.pop_front() {
                        count += 1;
                    } else {
                        thread::yield_now();
                    }
                }
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        assert!(deque.is_empty());
    }

    #[test]
    fn test_mixed_operations() {
        let deque: LockFreeDeque<i32, 6> = LockFreeDeque::new();

        // Mix front and back operations
        assert!(deque.push_front(1).is_ok());
        assert!(deque.push_back(2).is_ok());
        assert!(deque.push_front(0).is_ok());
        assert!(deque.push_back(3).is_ok());

        // Should be: [0, 1, 2, 3]
        assert_eq!(deque.pop_front(), Some(0));
        assert_eq!(deque.pop_back(), Some(3));
        assert_eq!(deque.pop_front(), Some(1));
        assert_eq!(deque.pop_back(), Some(2));
        assert!(deque.is_empty());
    }

    #[test]
    fn test_dequeue() {
        let deque = LockFreeDeque::<usize, 16>::new();
        for i in 0..4 {
            let _ = deque.push_front(i);
        }
        for _ in 0..18 {
            println!("{:?}", deque.pop_front());
        }

        // for _ in 0..5 {
        //     println!("{:?}", deque.alloc_node());
        // }
    }

    #[test]
    fn test_mpsc() {
        let pad = 64usize;

        let flag = Arc::new(AtomicI32::new(3));
        let flag1 = flag.clone();
        let flag2 = flag.clone();
        let flag3 = flag.clone();
        let p1 = Arc::new(LockFreeDeque::<usize, 256>::new());
        let p2 = p1.clone();
        let p3 = p1.clone();
        let c = p1.clone();

        let t1 = thread::spawn(move || {
            for i in 0..pad {
                let _ = p1.push_back(i);
            }
            flag1.fetch_sub(1, Ordering::SeqCst);
        });
        let t2 = thread::spawn(move || {
            for i in pad..(2 * pad) {
                let _ = p2.push_back(i);
            }
            flag2.fetch_sub(1, Ordering::SeqCst);
        });
        let t3 = thread::spawn(move || {
            for i in (2 * pad)..(3 * pad) {
                let _ = p3.push_back(i);
            }
            flag3.fetch_sub(1, Ordering::SeqCst);
        });

        let mut sum = 0;
        while flag.load(Ordering::SeqCst) != 0 || !c.is_empty() {
            if let Some(num) = c.pop_front() {
                sum += num;
            }
        }

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();
        assert_eq!(sum, (0..(3 * pad)).sum());
    }

    #[test]
    fn test_mpmc() {
        let pad = 64usize;

        let flag = Arc::new(AtomicI32::new(3));
        let flag_c = flag.clone();
        let flag1 = flag.clone();
        let flag2 = flag.clone();
        let flag3 = flag.clone();

        let p1 = Arc::new(LockFreeDeque::<usize, 256>::new());
        let p2 = p1.clone();
        let p3 = p1.clone();
        let c1 = p1.clone();
        let c2 = p1.clone();

        let producer1 = thread::spawn(move || {
            for i in 0..pad {
                let _ = p1.push_back(i);
            }
            flag1.fetch_sub(1, Ordering::SeqCst);
        });
        let producer2 = thread::spawn(move || {
            for i in pad..(2 * pad) {
                let _ = p2.push_back(i);
            }
            flag2.fetch_sub(1, Ordering::SeqCst);
        });
        let producer3 = thread::spawn(move || {
            for i in (2 * pad)..(3 * pad) {
                let _ = p3.push_back(i);
            }
            flag3.fetch_sub(1, Ordering::SeqCst);
        });

        let consumer = thread::spawn(move || {
            let mut sum = 0;
            while flag_c.load(Ordering::SeqCst) != 0 || !c2.is_empty() {
                if let Some(num) = c2.pop_front() {
                    sum += num;
                }
            }
            sum
        });

        let mut sum = 0;
        while flag.load(Ordering::SeqCst) != 0 || !c1.is_empty() {
            if let Some(num) = c1.pop_front() {
                sum += num;
            }
        }

        producer1.join().unwrap();
        producer2.join().unwrap();
        producer3.join().unwrap();

        let s = consumer.join().unwrap();
        sum += s;
        assert_eq!(sum, (0..(3 * pad)).sum());
    }

    #[test]
    fn test_mpmc_rev() {
        let pad = 64usize;

        let flag = Arc::new(AtomicI32::new(3));
        let flag_c = flag.clone();
        let flag1 = flag.clone();
        let flag2 = flag.clone();
        let flag3 = flag.clone();

        let p1 = Arc::new(LockFreeDeque::<usize, 256>::new());
        let p2 = p1.clone();
        let p3 = p1.clone();
        let c1 = p1.clone();
        let c2 = p1.clone();

        let producer1 = thread::spawn(move || {
            for i in 0..pad {
                let _ = p1.push_front(i);
            }
            flag1.fetch_sub(1, Ordering::SeqCst);
        });
        let producer2 = thread::spawn(move || {
            for i in pad..(2 * pad) {
                let _ = p2.push_front(i);
            }
            flag2.fetch_sub(1, Ordering::SeqCst);
        });
        let producer3 = thread::spawn(move || {
            for i in (2 * pad)..(3 * pad) {
                let _ = p3.push_front(i);
            }
            flag3.fetch_sub(1, Ordering::SeqCst);
        });

        let consumer = thread::spawn(move || {
            let mut sum = 0;
            while flag_c.load(Ordering::SeqCst) != 0 || !c2.is_empty() {
                if let Some(num) = c2.pop_back() {
                    sum += num;
                }
            }
            sum
        });

        let mut sum = 0;
        while flag.load(Ordering::SeqCst) != 0 || !c1.is_empty() {
            if let Some(num) = c1.pop_back() {
                sum += num;
            }
        }

        producer1.join().unwrap();
        producer2.join().unwrap();
        producer3.join().unwrap();

        let s = consumer.join().unwrap();
        sum += s;
        assert_eq!(sum, (0..(3 * pad)).sum());
    }

    // this test may take a long time to finish (≈ 1 minute)
    // significantly longer than that means there is probably a deadlock
    #[test]
    fn test_mpmc_mix() {
        let mut count = 10000;
        while count > 0 {
            count -= 1;
            let pad = 750usize;

            let flag = Arc::new(AtomicI32::new(4));
            let flag_c = flag.clone();
            let flag1 = flag.clone();
            let flag2 = flag.clone();
            let flag3 = flag.clone();
            let flag4 = flag.clone();

            let p1 = Arc::new(LockFreeDeque::<usize, 4096>::new());
            let p2 = p1.clone();
            let p3 = p1.clone();
            let p4 = p1.clone();
            let c1 = p1.clone();
            let c2 = p1.clone();

            let producer1 = thread::spawn(move || {
                for i in 0..pad {
                    if let Err(item) = p1.push_front(i) {
                        println!("Failed to push front {}", item);
                    }
                    // if let Err(item) = p1.push_back(i) {
                    //     println!("Failed to push back {}", item);
                    // }
                }
                flag1.fetch_sub(1, Ordering::SeqCst);
            });
            let producer2 = thread::spawn(move || {
                for i in pad..(2 * pad) {
                    // if let Err(item) = p2.push_front(i) {
                    //     println!("Failed to push front {}", item);
                    // }
                    if let Err(item) = p2.push_back(i) {
                        println!("Failed to push back {}", item);
                    }
                }
                flag2.fetch_sub(1, Ordering::SeqCst);
            });
            let producer3 = thread::spawn(move || {
                for i in (2 * pad)..(3 * pad) {
                    if let Ok(mut guard) = p3.push_slot_front() {
                        guard.write(i);
                    } else {
                        println!("Failed to push front {}", i);
                    }
                    // if let Ok(mut guard) = p3.push_slot_back() {
                    //     guard.write(i);
                    // } else {
                    //     println!("Failed to push front {}", i);
                    // }
                }
                flag3.fetch_sub(1, Ordering::SeqCst);
            });
            let producer4 = thread::spawn(move || {
                for i in (3 * pad)..(4 * pad) {
                    // if let Ok(mut guard) = p4.push_slot_front() {
                    //     guard.write(i);
                    // } else {
                    //     println!("Failed to push front {}", i);
                    // }
                    if let Ok(mut guard) = p4.push_slot_back() {
                        guard.write(i);
                    } else {
                        println!("Failed to push front {}", i);
                    }
                }
                flag4.fetch_sub(1, Ordering::SeqCst);
            });

            let consumer = thread::spawn(move || {
                let mut sum = 0;
                while flag_c.load(Ordering::SeqCst) != 0 || !c2.is_empty() {
                    if let Some(num) = c2.pop_front() {
                        // if let Some(num) = c2.pop_back() {
                        sum += num;
                    }
                }
                sum
            });

            let mut sum = 0;
            while flag.load(Ordering::SeqCst) != 0 || !c1.is_empty() {
                // if let Some(num) = c1.pop_front() {
                if let Some(num) = c1.pop_back() {
                    sum += num;
                }
            }

            producer1.join().unwrap();
            producer2.join().unwrap();
            producer3.join().unwrap();
            producer4.join().unwrap();

            let s = consumer.join().unwrap();
            sum += s;
            assert_eq!(sum, (0..(4 * pad)).sum());
        }
    }

    // this test may take a long time to finish (< 1 minute)
    // longer than that means there is probably a deadlock
    //
    // currently, this test will deadlock because of an unsolved bug.
    #[test]
    fn test_mpmc_full_mix() {
        let mut count = 10000;
        while count > 0 {
            count -= 1;
            let pad = 1000usize;

            let flag = Arc::new(AtomicI32::new(3));
            let flag_c = flag.clone();
            let flag1 = flag.clone();
            let flag2 = flag.clone();
            let flag3 = flag.clone();

            let p1 = Arc::new(LockFreeDeque::<usize, 4096>::new());
            let p2 = p1.clone();
            let p3 = p1.clone();
            let c1 = p1.clone();
            let c2 = p1.clone();

            // Fill the deque until it is full
            for _ in 0..4095 {
                if let Err(item) = p1.push_front(0) {
                    println!("Failed to push front {}", item);
                }
            }

            let producer1 = thread::spawn(move || {
                for i in 0..pad {
                    while p1.push_front(i).is_err() {}
                    // while p1.push_back(i).is_err() {}
                }
                flag1.fetch_sub(1, Ordering::SeqCst);
            });
            let producer2 = thread::spawn(move || {
                for i in pad..(2 * pad) {
                    // while p2.push_front(i).is_err() {}
                    while p2.push_back(i).is_err() {}
                }
                flag2.fetch_sub(1, Ordering::SeqCst);
            });
            let producer3 = thread::spawn(move || {
                for i in (2 * pad)..(3 * pad) {
                    while p3.push_front(i).is_err() {}
                    // while p3.push_back(i).is_err() {}
                }
                flag3.fetch_sub(1, Ordering::SeqCst);
            });

            let consumer = thread::spawn(move || {
                let mut sum = 0;
                while flag_c.load(Ordering::SeqCst) != 0 || !c2.is_empty() {
                    if let Some(num) = c2.pop_front() {
                        // if let Some(num) = c2.pop_back() {
                        sum += num;
                    }
                }
                sum
            });

            let mut sum = 0;
            while flag.load(Ordering::SeqCst) != 0 || !c1.is_empty() {
                // if let Some(num) = c1.pop_front() {
                if let Some(num) = c1.pop_back() {
                    sum += num;
                }
            }

            producer1.join().unwrap();
            producer2.join().unwrap();
            producer3.join().unwrap();

            let s = consumer.join().unwrap();
            sum += s;
            assert_eq!(sum, (0..(3 * pad)).sum());
        }
    }

    #[test]
    fn test_push_pop() {
        const WORKERS_PER_QUEUE: usize = 16;
        const DATA_PER_WORKER: usize = 128;

        let mut handles = vec::Vec::new();
        let queue = Arc::new(LockFreeDeque::<usize, 4097>::new());

        for worker_id in 0..WORKERS_PER_QUEUE {
            let queue_c = queue.clone();
            // let data_num_c = data_num.clone();
            let handle = std::thread::spawn(move || {
                for i in 0..DATA_PER_WORKER {
                    queue_c.push_front(i).expect(
                        std::format!("Failed to push data in worker {}, iter {}", worker_id, i)
                            .as_str(),
                    );
                    // data_num_c.fetch_add(1, Ordering::AcqRel);
                }
                for i in 0..DATA_PER_WORKER {
                    // let data_num = data_num_c.fetch_sub(1, Ordering::AcqRel);
                    // if data_num < 0 {
                    //     println!("data_num < 0 in queue {}, worker {}", queue_id, worker_id);
                    //     while data_num_c.load(Ordering::Acquire) < 0 {}
                    // }
                    queue_c.pop_back().expect(
                        std::format!("Failed to pop data in worker {}, iter {}", worker_id, i)
                            .as_str(),
                    );
                    // let data = pop(queue_id).expect(
                    //     std::format!(
                    //         "Failed to pop data in queue {}, worker {}",
                    //         queue_id,
                    //         worker_id
                    //     )
                    //     .as_str(),
                    // );
                    // assert!(data.msg_type == 0);
                }
            });
            handles.push(handle);
        }
    }
}
