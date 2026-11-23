#![doc = include_str!("../README.md")]

pub mod adapter;
mod catalog;
pub mod native;
pub mod rpc;
mod session;

pub use adapter::{Adapter, Config, SpawnPlan, ToolPosture};
