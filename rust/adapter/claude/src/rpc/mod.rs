mod client;
mod codec;
mod message;

use std::fmt;

pub use client::{Client, ClientError, ClientOptions, Closer, DEFAULT_QUEUE_CAPACITY};
pub use codec::{Decoder, Encoder, DEFAULT_FRAME_LIMIT};
pub use message::{
    control_request_message, control_response_message, parse_message, user_turn_message,
    ControlResponseEnvelope, Kind, Message,
};

#[derive(Debug)]
pub enum Fault {
    InvalidMessage(String),
    InvalidControl(String),
    FrameTooLarge,
    EndOfStream,
    Io(std::io::Error),
}

#[derive(Debug)]
pub struct Error {
    pub fault: Fault,
}

fn trailing(detail: &str) -> String {
    if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    }
}

impl Error {
    pub(crate) fn invalid(detail: impl Into<String>) -> Self {
        Self {
            fault: Fault::InvalidMessage(detail.into()),
        }
    }

    pub(crate) fn control(detail: impl Into<String>) -> Self {
        Self {
            fault: Fault::InvalidControl(detail.into()),
        }
    }

    pub(crate) fn frame_too_large() -> Self {
        Self {
            fault: Fault::FrameTooLarge,
        }
    }

    pub(crate) fn end_of_stream() -> Self {
        Self {
            fault: Fault::EndOfStream,
        }
    }

    pub(crate) fn io(error: std::io::Error) -> Self {
        Self {
            fault: Fault::Io(error),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.fault {
            Fault::InvalidMessage(detail) => {
                write!(
                    f,
                    "claude rpc: invalid stream-json message{}",
                    trailing(detail)
                )
            }
            Fault::InvalidControl(detail) => {
                write!(
                    f,
                    "claude rpc: invalid control-plane message{}",
                    trailing(detail)
                )
            }
            Fault::FrameTooLarge => f.write_str("claude rpc: frame exceeds configured limit"),
            Fault::EndOfStream => f.write_str("EOF"),
            Fault::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {}
