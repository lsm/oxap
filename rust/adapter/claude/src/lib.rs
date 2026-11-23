#![doc = include_str!("../README.md")]

pub mod adapter;
pub mod native;
pub mod rpc;
mod session;

pub use adapter::{Adapter, Config, SpawnPlan, ToolPosture};
