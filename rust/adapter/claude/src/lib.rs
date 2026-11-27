#![doc = include_str!("../README.md")]

pub mod adapter;
mod catalog;
pub mod native;
mod native_list;
mod native_read;
mod provider;
pub mod rpc;
mod session;

pub use adapter::{Adapter, Config, SpawnPlan, ToolPosture};
pub use provider::{AuthMode, Header, Provider};
