//! The core functionality of a monolithic kernel, including loading user
//! programs and managing processes.

#![no_std]
#![feature(likely_unlikely)]
#![warn(missing_docs)]

extern crate alloc;

extern crate libvqueue;
extern crate libvsched2;

#[macro_use]
extern crate axlog;

pub mod config;
pub mod futex;
pub mod mm;
pub mod resources;
pub mod shm;
pub mod task;
pub mod time;
pub mod vfs;
// pub mod vipc;
#[allow(missing_docs)]
pub mod vsched;
