#![doc = include_str!("../README.md")]

#[cfg(unix)]
mod gateway_session;
#[cfg(unix)]
mod prompt_observation;

#[cfg(unix)]
pub use gateway_session::{GatewaySession, Observation};
#[cfg(unix)]
pub use prompt_observation::PromptObservation;

pub mod native;
#[cfg(unix)]
pub mod process;
pub mod rpc;
