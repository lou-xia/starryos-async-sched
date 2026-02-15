use alloc::sync::Arc;
use core::task::Waker;

use linked_list::{GetLinks, Links, List};

/// A waker wrapper.
///
/// It add extra states to use in [`linked_list::List`].
pub struct WaitWakerNode {
    waker: Waker,
    links: Links<Self>,
}

impl GetLinks for WaitWakerNode {
    type EntryType = Self;

    #[inline]
    fn get_links(t: &Self) -> &Links<Self> {
        &t.links
    }
}

impl WaitWakerNode {
    /// Creates a new waker.
    pub const fn new(waker: Waker) -> Self {
        Self {
            waker,
            links: Links::new(),
        }
    }
}

/// A simple FIFO wait waker list
///
/// When a waker is added to the list, it's placed at the end of the waitlist.
/// When picking the next waker to run, the head of the wait list is taken.
pub struct WaitTaskList {
    list: List<Arc<WaitWakerNode>>,
}

impl WaitTaskList {
    /// Creates a new empty [WaitList].
    pub const fn new() -> Self {
        Self { list: List::new() }
    }

    /// Register a waker to the list.
    pub fn prepare_to_wait(&mut self, waker: Arc<WaitWakerNode>) {
        let task = waker.waker.data() as *const crate::Task;
        unsafe { &*task }.set_state(crate::task::TaskState::Blocking);
        self.list.push_back(waker);
    }

    /// Removes the given Node
    ///
    /// # Safety
    ///
    /// Callers must ensure that `data` is either on this list or in no list. It being on another
    /// list leads to memory unsafety.
    pub fn remove(&mut self, node: &Arc<WaitWakerNode>) -> Option<Arc<WaitWakerNode>> {
        unsafe { self.list.remove(node) }
    }

    /// notify special task and remove it
    pub fn notify_task(&mut self, waker: &Waker) -> bool {
        let mut cursor = self.list.cursor_front_mut();
        let wake = loop {
            match cursor.current() {
                Some(node) => {
                    if node.waker.will_wake(waker) {
                        node.waker.wake_by_ref();
                        break true;
                    }
                }
                None => break false,
            }
            cursor.move_next();
        };
        if wake {
            cursor.remove_current();
        }

        false
    }

    /// notify first task and remove it
    pub fn notify_one(&mut self) -> bool {
        if let Some(node) = self.list.pop_front() {
            node.waker.wake_by_ref();
            return true;
        }
        false
    }

    /// notify all task and remove it
    pub fn notify_all(&mut self) {
        loop {
            if !self.notify_one() {
                break;
            }
        }
    }
}
