use std::fmt;
use std::io::{Read, Write};
use std::time::Instant;

use crossbeam_channel::Receiver;
use serde::Serialize;
use serde_json::value::RawValue;

use crate::native::{self, Message};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Closed,
    Backpressure,
    DeadlineExceeded,
    Remote { code: i64, message: String },
    Wire,
    Transport(String),
    Encode(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("Copilot RPC closed"),
            Self::Backpressure => f.write_str("Copilot RPC queue is full"),
            Self::DeadlineExceeded => f.write_str("context deadline exceeded"),
            Self::Remote { code, message } => write!(f, "Copilot RPC error {code}: {message}"),
            Self::Wire => native::WireError.fmt(f),
            Self::Transport(detail) | Self::Encode(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for Error {}

pub type Closer = Box<dyn FnOnce() + Send>;

#[derive(Clone)]
pub struct Client {
    notes: Receiver<Message>,
    done: Receiver<()>,
}

impl Client {
    pub fn new(
        _reader: impl Read + Send + 'static,
        _writer: impl Write + Send + 'static,
        _closer: Closer,
        _limit: usize,
        _capacity: usize,
    ) -> Self {
        Self {
            notes: crossbeam_channel::never(),
            done: crossbeam_channel::never(),
        }
    }

    pub fn call<P: Serialize>(
        &self,
        _method: &str,
        _params: &P,
        _deadline: Option<Instant>,
    ) -> Result<Option<Box<RawValue>>, Error> {
        Err(Error::Closed)
    }

    pub fn notifications(&self) -> Receiver<Message> {
        self.notes.clone()
    }

    pub fn done(&self) -> Receiver<()> {
        self.done.clone()
    }

    pub fn err(&self) -> Option<Error> {
        None
    }

    pub fn close(&self) {}
}
