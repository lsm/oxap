use std::collections::HashMap;
use std::fmt;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crossbeam_channel::{after, never, select, Receiver, Sender, TrySendError};
use oxap_adapter::gojson::go_marshal;
use serde::Serialize;
use serde_json::value::RawValue;

use crate::native::{self, Kind, Message};
use crate::rpc::{Decoder, Encoder};

const DEFAULT_CAPACITY: usize = 64;

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

struct Inner {
    ids: AtomicI64,
    pending: Mutex<HashMap<String, Sender<Message>>>,
    err: Mutex<Option<Error>>,
    notes: Receiver<Message>,
    writes: Sender<Vec<u8>>,
    done: Receiver<()>,
    open: Mutex<Option<Sender<()>>>,
    closer: Mutex<Option<Closer>>,
    stopped: Receiver<()>,
}

#[derive(Clone)]
pub struct Client(Arc<Inner>);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[derive(Serialize)]
struct Call<'a, P> {
    jsonrpc: &'static str,
    id: i64,
    method: &'a str,
    params: &'a P,
}

impl Client {
    pub fn new(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        closer: Closer,
        limit: usize,
        capacity: usize,
    ) -> Self {
        let capacity = if capacity == 0 {
            DEFAULT_CAPACITY
        } else {
            capacity
        };
        let (note_tx, notes) = crossbeam_channel::bounded(capacity);
        let (writes, write_rx) = crossbeam_channel::bounded(capacity);
        let (open, done) = crossbeam_channel::bounded(0);
        let (alive, stopped) = crossbeam_channel::bounded::<()>(0);
        let client = Self(Arc::new(Inner {
            ids: AtomicI64::new(0),
            pending: Mutex::new(HashMap::new()),
            err: Mutex::new(None),
            notes,
            writes,
            done,
            open: Mutex::new(Some(open)),
            closer: Mutex::new(Some(closer)),
            stopped,
        }));
        let (reading, writing) = (client.clone(), client.clone());
        let reader_alive = alive.clone();
        std::thread::spawn(move || {
            let _alive = reader_alive;
            reading.read(Decoder::new(reader, limit), note_tx);
        });
        std::thread::spawn(move || {
            let _alive = alive;
            writing.write(Encoder::new(writer, limit), write_rx);
        });
        client
    }

    pub fn call<P: Serialize>(
        &self,
        method: &str,
        params: &P,
        deadline: Option<Instant>,
    ) -> Result<Option<Box<RawValue>>, Error> {
        if deadline.is_some_and(|at| Instant::now() >= at) {
            return Err(Error::DeadlineExceeded);
        }
        let id = self.0.ids.fetch_add(1, Ordering::SeqCst) + 1;
        let body = serde_json::to_string(&Call {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })
        .map_err(|failure| Error::Encode(failure.to_string()))?;
        let key = id.to_string();
        let (reply, replies) = crossbeam_channel::bounded(1);
        lock(&self.0.pending).insert(key.clone(), reply);
        let outcome = self.exchange(go_marshal(&body).into_bytes(), &replies, deadline);
        lock(&self.0.pending).remove(&key);
        outcome
    }

    fn exchange(
        &self,
        body: Vec<u8>,
        replies: &Receiver<Message>,
        deadline: Option<Instant>,
    ) -> Result<Option<Box<RawValue>>, Error> {
        let expiry = deadline.map_or_else(never, |at| {
            after(at.saturating_duration_since(Instant::now()))
        });
        select! {
            send(self.0.writes, body) -> _ => {}
            recv(expiry) -> _ => return Err(self.expire()),
            recv(self.0.done) -> _ => return Err(self.closed_error()),
        }
        select! {
            recv(replies) -> reply => match reply {
                Ok(reply) => decode_reply(reply),
                Err(_) => Err(self.closed_error()),
            },
            recv(expiry) -> _ => match replies.try_recv() {
                Ok(reply) => decode_reply(reply),
                Err(_) => Err(self.expire()),
            },
            recv(self.0.done) -> _ => match replies.try_recv() {
                Ok(reply) => decode_reply(reply),
                Err(_) => Err(self.closed_error()),
            },
        }
    }

    fn expire(&self) -> Error {
        self.fail(Error::DeadlineExceeded);
        Error::DeadlineExceeded
    }

    fn closed_error(&self) -> Error {
        self.err().unwrap_or(Error::Closed)
    }

    fn read<R: Read>(&self, mut decoder: Decoder<R>, notes: Sender<Message>) {
        loop {
            let message = match decoder.read() {
                Ok(body) => match native::parse(&body) {
                    Ok(message) => message,
                    Err(_) => return self.fail(Error::Wire),
                },
                Err(failure) => return self.fail(Error::Transport(failure.to_string())),
            };
            match message.kind {
                Kind::Response | Kind::Failure => {
                    let key = message.id.as_deref().map_or("", RawValue::get);
                    let Some(reply) = lock(&self.0.pending).remove(key) else {
                        return self.fail(Error::Wire);
                    };
                    let _ = reply.send(message);
                }
                Kind::Notification => {
                    if let Err(TrySendError::Full(_)) = notes.try_send(message) {
                        return self.fail(Error::Backpressure);
                    }
                }
                Kind::Request => {
                    let id = message.id.as_deref().map_or("null", RawValue::get);
                    let refusal = format!(
                        r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32601,"message":"reverse requests are not enabled"}}}}"#
                    );
                    let refusal = go_marshal(&refusal).into_bytes();
                    if let Err(TrySendError::Full(_)) = self.0.writes.try_send(refusal) {
                        return self.fail(Error::Backpressure);
                    }
                }
            }
        }
    }

    fn write<W: Write>(&self, mut encoder: Encoder<W>, bodies: Receiver<Vec<u8>>) {
        loop {
            select! {
                recv(bodies) -> body => {
                    let Ok(body) = body else { return };
                    if let Err(failure) = encoder.write(&body) {
                        return self.fail(Error::Transport(failure.to_string()));
                    }
                }
                recv(self.0.done) -> _ => return,
            }
        }
    }

    fn fail(&self, error: Error) {
        let Some(open) = lock(&self.0.open).take() else {
            return;
        };
        *lock(&self.0.err) = Some(error);
        drop(open);
        if let Some(closer) = lock(&self.0.closer).take() {
            closer();
        }
    }

    pub fn notifications(&self) -> Receiver<Message> {
        self.0.notes.clone()
    }

    pub fn done(&self) -> Receiver<()> {
        self.0.done.clone()
    }

    pub fn err(&self) -> Option<Error> {
        lock(&self.0.err).clone()
    }

    pub fn close(&self) {
        self.fail(Error::Closed);
        let _ = self.0.stopped.recv();
    }
}

fn decode_reply(reply: Message) -> Result<Option<Box<RawValue>>, Error> {
    if reply.kind == Kind::Failure {
        return Err(Error::Remote {
            code: reply.code,
            message: reply.error,
        });
    }
    Ok(reply.result)
}
