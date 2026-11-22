#![doc = include_str!("../README.md")]

#[cfg(unix)]
mod gateway_session;

#[cfg(unix)]
pub use gateway_session::GatewaySession;

pub mod native;
#[cfg(unix)]
pub mod process;
pub mod rpc;
