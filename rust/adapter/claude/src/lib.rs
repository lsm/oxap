#![doc = include_str!("../README.md")]

pub mod adapter;
pub mod native;
pub mod rpc;

pub use adapter::{Adapter, Config, SpawnPlan, ToolPosture};
