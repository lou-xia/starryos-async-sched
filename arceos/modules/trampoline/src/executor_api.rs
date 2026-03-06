use alloc::{string::String, vec::Vec};
use axerrno::AxResult;
use executor::executor::Executor;
use asynctask::TaskRef;

pub async fn init_user(args: Vec<String>, envs: &Vec<String>) -> AxResult<TaskRef> {
    Executor::init_user(args, envs).await
}
