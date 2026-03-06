use alloc::{collections::btree_map::BTreeMap, sync::Arc};
use asynctask::TaskRef;
use axsync::Mutex;

use crate::executor::Executor;

pub static TID2TASK: Mutex<BTreeMap<usize, TaskRef>> = Mutex::new(BTreeMap::new());
pub static PID2PC: Mutex<BTreeMap<usize, Arc<Executor>>> = Mutex::new(BTreeMap::new());
