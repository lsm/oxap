use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::io::{BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use crossbeam_channel::{after, never, select, Receiver, Sender, TrySendError};
use serde_json::value::RawValue;

use crate::native::{decode_control_request, decode_observation, ControlRequest, Observation};

use super::{
    control_request_message, control_response_message, user_turn_message, Decoder, Encoder, Kind,
    Message, DEFAULT_FRAME_LIMIT,
};

pub const DEFAULT_QUEUE_CAPACITY: usize = 64;
pub const DEFAULT_QUEUE_STALL: Duration = Duration::from_secs(30);

pub type Closer = Box<dyn FnOnce() + Send>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientError {
    Closed,
    ControlQueue,
    ObservationQueue,
    WriteQueue,
    ReverseResolved,
    DeadlineExceeded,
    DuplicateRequestId(String),
    Remote { id: String, detail: String },
    Transport(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("claude rpc: client closed"),
            Self::ControlQueue => f.write_str("claude rpc: reverse control queue is full"),
            Self::ObservationQueue => f.write_str("claude rpc: observation queue is full"),
            Self::WriteQueue => f.write_str("claude rpc: write queue is full"),
            Self::ReverseResolved => {
                f.write_str("claude rpc: reverse control request already resolved")
            }
            Self::DeadlineExceeded => f.write_str("context deadline exceeded"),
            Self::DuplicateRequestId(id) => {
                write!(f, "claude rpc: request id is already active: {id}")
            }
            Self::Remote { id, detail } => {
                write!(f, "claude rpc error for request {id}: {detail}")
            }
            Self::Transport(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for ClientError {}

#[derive(Default)]
pub struct ClientOptions {
    pub frame_limit: usize,
    pub queue_capacity: usize,
    pub write_queue_capacity: usize,
    pub queue_stall: Option<Duration>,
    pub closer: Option<Closer>,
}

#[derive(Clone, Debug)]
pub struct ObservationMessage {
    pub message_type: String,
    pub subtype: String,
    pub raw: Vec<u8>,
    pub value: Observation,
}

#[derive(Debug)]
pub enum Inbound {
    Observation(ObservationMessage),
    Control(Arc<IncomingControl>),
    Cancel(CancelNotice),
    Barrier(Sender<()>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelNotice {
    pub request_id: String,
}

pub struct IncomingControl {
    pub id: String,
    pub subtype: String,
    pub raw: Vec<u8>,
    pub value: ControlRequest,
    client: Weak<Inner>,
    done: Mutex<bool>,
}

impl fmt::Debug for IncomingControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IncomingControl")
            .field("id", &self.id)
            .field("subtype", &self.subtype)
            .finish_non_exhaustive()
    }
}

impl IncomingControl {
    pub fn respond(
        &self,
        response: Option<&RawValue>,
        deadline: Option<Instant>,
    ) -> Result<(), ClientError> {
        let body = response.map_or("{}", RawValue::get);
        self.answer(
            &format!(r#"{{"response":{body},"subtype":"success""#),
            deadline,
        )
    }

    pub fn respond_error(
        &self,
        message: &str,
        deadline: Option<Instant>,
    ) -> Result<(), ClientError> {
        let error = serde_json::Value::from(message);
        self.answer(&format!(r#"{{"error":{error},"subtype":"error""#), deadline)
    }

    fn answer(&self, head: &str, deadline: Option<Instant>) -> Result<(), ClientError> {
        let mut done = lock(&self.done);
        if *done {
            return Err(ClientError::ReverseResolved);
        }
        let id = serde_json::Value::from(self.id.as_str());
        let envelope = format!(r#"{head},"request_id":{id}}}"#);
        let client = Client(self.client.upgrade().ok_or(ClientError::Closed)?);
        client.write(control_response_message(envelope.as_bytes()), deadline)?;
        *done = true;
        let mut state = lock(&client.0.state);
        if state
            .incoming
            .get(&self.id)
            .is_some_and(|current| std::ptr::eq(Arc::as_ptr(current), self))
        {
            state.incoming.remove(&self.id);
        }
        Ok(())
    }
}

struct WriteRequest {
    message: Message,
    deadline: Option<Instant>,
    started: Sender<()>,
    encoded: Sender<()>,
    result: Sender<Result<(), ClientError>>,
}

type CallResult = Result<Option<Vec<u8>>, ClientError>;

struct State {
    pending: HashMap<String, Sender<CallResult>>,
    incoming: HashMap<String, Arc<IncomingControl>>,
    decoding: bool,
    routing: bool,
    closed: bool,
    err: Option<ClientError>,
    reader_err: Option<ClientError>,
}

struct Inner {
    state: Mutex<State>,
    counter: AtomicU64,
    entropy: RandomState,
    writes: Sender<WriteRequest>,
    stall: Duration,
    inbound_rx: Receiver<Inbound>,
    done_rx: Receiver<()>,
    done_tx: Mutex<Option<Sender<()>>>,
    read_done: Receiver<()>,
    closer: Mutex<Option<Closer>>,
    closing: AtomicBool,
    closed_tx: Mutex<Option<Sender<()>>>,
    closed_rx: Receiver<()>,
    unblocks_writes: bool,
}

#[derive(Clone)]
pub struct Client(Arc<Inner>);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn or_default(value: usize, default: usize) -> usize {
    if value == 0 {
        default
    } else {
        value
    }
}

fn deadline_channel(deadline: Option<Instant>) -> Receiver<Instant> {
    match deadline {
        Some(at) => after(at.saturating_duration_since(Instant::now())),
        None => never(),
    }
}

impl Client {
    pub fn new(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        mut options: ClientOptions,
    ) -> Self {
        let capacity = or_default(options.queue_capacity, DEFAULT_QUEUE_CAPACITY);
        let (writes, write_rx) =
            crossbeam_channel::bounded(or_default(options.write_queue_capacity, capacity));
        let (inbound_tx, inbound_rx) = crossbeam_channel::bounded(capacity);
        let (done_tx, done_rx) = crossbeam_channel::bounded(0);
        let (reading, read_done) = crossbeam_channel::bounded::<()>(0);
        let (closed_tx, closed_rx) = crossbeam_channel::bounded::<()>(0);
        let unblocks_writes = options.closer.is_some();
        let client = Self(Arc::new(Inner {
            state: Mutex::new(State {
                pending: HashMap::new(),
                incoming: HashMap::new(),
                decoding: false,
                routing: false,
                closed: false,
                err: None,
                reader_err: None,
            }),
            counter: AtomicU64::new(0),
            entropy: RandomState::new(),
            writes,
            stall: options
                .queue_stall
                .filter(|stall| !stall.is_zero())
                .unwrap_or(DEFAULT_QUEUE_STALL),
            inbound_rx,
            done_rx,
            done_tx: Mutex::new(Some(done_tx)),
            read_done,
            closer: Mutex::new(options.closer.take()),
            closing: AtomicBool::new(false),
            closed_tx: Mutex::new(Some(closed_tx)),
            closed_rx,
            unblocks_writes,
        }));
        let limit = or_default(options.frame_limit, DEFAULT_FRAME_LIMIT);
        let writer_client = client.clone();
        std::thread::spawn(move || writer_client.write_loop(Encoder::new(writer, limit), write_rx));
        let reader_client = client.clone();
        std::thread::spawn(move || {
            let _reading = reading;
            reader_client.read_loop(Decoder::new(BufReader::new(reader), limit), inbound_tx)
        });
        client
    }

    pub fn inbound(&self) -> Receiver<Inbound> {
        self.0.inbound_rx.clone()
    }

    pub fn done(&self) -> Receiver<()> {
        self.0.done_rx.clone()
    }

    pub fn read_done(&self) -> Receiver<()> {
        self.0.read_done.clone()
    }

    pub fn err(&self) -> Option<ClientError> {
        let state = lock(&self.0.state);
        state.reader_err.clone().or_else(|| state.err.clone())
    }

    pub fn close(&self) {
        self.close_with(ClientError::Closed);
    }

    pub fn call(&self, request: &[u8], deadline: Option<Instant>) -> CallResult {
        let id = self.mint_id();
        let (respond, response) = crossbeam_channel::bounded(1);
        {
            let mut state = lock(&self.0.state);
            if state.closed {
                drop(state);
                return Err(self.close_error());
            }
            if state.pending.contains_key(&id) {
                return Err(ClientError::DuplicateRequestId(id));
            }
            state.pending.insert(id.clone(), respond);
        }
        if let Err(error) = self.write(control_request_message(&id, request), deadline) {
            if !self.is_closed() {
                lock(&self.0.state).pending.remove(&id);
                return Err(error);
            }
        }
        select! {
            recv(response) -> outcome => outcome.unwrap_or_else(|_| Err(self.close_error())),
            recv(deadline_channel(deadline)) -> _ => {
                if let Ok(outcome) = response.try_recv() {
                    return outcome;
                }
                lock(&self.0.state).pending.remove(&id);
                self.close_with(ClientError::DeadlineExceeded);
                Err(ClientError::DeadlineExceeded)
            }
        }
    }

    fn mint_id(&self) -> String {
        let count = self
            .0
            .counter
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        let suffix = self.0.entropy.hash_one(count) as u32;
        format!("req_{count}_{suffix:08x}")
    }

    pub fn write_user(&self, frame: &[u8], deadline: Option<Instant>) -> Result<(), ClientError> {
        self.write(user_turn_message(frame), deadline)
    }

    fn is_closed(&self) -> bool {
        lock(&self.0.state).closed
    }

    fn close_error(&self) -> ClientError {
        self.err().unwrap_or(ClientError::Closed)
    }

    fn close_with(&self, reason: ClientError) {
        if self.0.closing.swap(true, Ordering::SeqCst) {
            let _ = self.0.closed_rx.recv();
            return;
        }
        if let Some(closer) = lock(&self.0.closer).take() {
            closer();
        }
        let pending = {
            let mut state = lock(&self.0.state);
            state.closed = true;
            state.err.get_or_insert(reason);
            state.incoming.clear();
            if state.routing || (state.decoding && self.0.unblocks_writes) {
                HashMap::new()
            } else {
                std::mem::take(&mut state.pending)
            }
        };
        lock(&self.0.done_tx).take();
        lock(&self.0.closed_tx).take();
        self.fail_pending(pending);
    }

    fn fail_pending(&self, pending: HashMap<String, Sender<CallResult>>) {
        let error = self.close_error();
        for (_, respond) in pending {
            let _ = respond.send(Err(error.clone()));
        }
    }

    fn write(&self, message: Message, deadline: Option<Instant>) -> Result<(), ClientError> {
        if deadline.is_some_and(|at| Instant::now() >= at) {
            return Err(ClientError::DeadlineExceeded);
        }
        if self.is_closed() {
            return Err(self.close_error());
        }
        let (started, began) = crossbeam_channel::bounded(1);
        let (encoded, settled) = crossbeam_channel::bounded(1);
        let (result, written) = crossbeam_channel::bounded(1);
        match self.0.writes.try_send(WriteRequest {
            message,
            deadline,
            started,
            encoded,
            result,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(ClientError::WriteQueue),
            Err(TrySendError::Disconnected(_)) => return Err(self.close_error()),
        }
        let mut done = self.0.done_rx.clone();
        loop {
            select! {
                recv(written) -> outcome => {
                    return outcome.unwrap_or_else(|_| Err(self.close_error()))
                }
                recv(deadline_channel(deadline)) -> _ => {
                    self.close_with(ClientError::DeadlineExceeded);
                    return Err(ClientError::DeadlineExceeded);
                }
                recv(done) -> _ => {
                    let settles = settled.try_recv().is_ok() || self.0.unblocks_writes;
                    if !(began.try_recv().is_ok() && settles) {
                        return Err(self.close_error());
                    }
                    done = never();
                }
            }
        }
    }

    fn write_loop<W: Write>(&self, mut encoder: Encoder<W>, requests: Receiver<WriteRequest>) {
        loop {
            select! {
                recv(requests) -> request => {
                    let Ok(request) = request else { return };
                    if self.is_closed() {
                        let _ = request.result.send(Err(self.close_error()));
                        return;
                    }
                    if request.deadline.is_some_and(|at| Instant::now() >= at) {
                        let _ = request.result.send(Err(ClientError::DeadlineExceeded));
                        continue;
                    }
                    let _ = request.started.send(());
                    let outcome = encoder
                        .encode(&request.message)
                        .map_err(|error| ClientError::Transport(error.to_string()));
                    let _ = request.encoded.send(());
                    let failed = outcome.is_err();
                    if let Err(reason) = &outcome {
                        self.close_with(reason.clone());
                    }
                    let _ = request.result.send(outcome);
                    if failed {
                        return;
                    }
                }
                recv(self.0.done_rx) -> _ => return,
            }
        }
    }

    fn read_loop<R: std::io::BufRead>(&self, mut decoder: Decoder<R>, inbound: Sender<Inbound>) {
        let error = loop {
            lock(&self.0.state).decoding = true;
            let decoded = decoder.decode();
            {
                let mut state = lock(&self.0.state);
                state.decoding = false;
                state.routing = decoded.is_ok();
            }
            let message = match decoded {
                Ok(message) => message,
                Err(error) => break error,
            };
            let stop = self.route(message, &inbound);
            self.settle_reader();
            if stop {
                return;
            }
        };
        let reason = ClientError::Transport(error.to_string());
        lock(&self.0.state).reader_err = Some(reason.clone());
        self.close_with(reason);
        self.settle_reader();
    }

    fn settle_reader(&self) {
        let pending = {
            let mut state = lock(&self.0.state);
            state.routing = false;
            if state.closed {
                std::mem::take(&mut state.pending)
            } else {
                HashMap::new()
            }
        };
        self.fail_pending(pending);
    }

    fn route(&self, message: Message, inbound: &Sender<Inbound>) -> bool {
        match message.kind {
            Kind::ControlResponse => {
                let Some(envelope) = message.response else {
                    return false;
                };
                let pending = lock(&self.0.state)
                    .pending
                    .contains_key(&envelope.request_id);
                if pending && !self.barrier(inbound) {
                    return true;
                }
                let outcome = if envelope.success {
                    Ok(envelope.response)
                } else {
                    Err(ClientError::Remote {
                        id: envelope.request_id.clone(),
                        detail: envelope.error,
                    })
                };
                if let Some(respond) = lock(&self.0.state).pending.remove(&envelope.request_id) {
                    let _ = respond.send(outcome);
                }
                false
            }
            Kind::Observation => {
                match decode_observation(&message.message_type, &message.subtype, &message.raw) {
                    Ok(value) => !self.enqueue(
                        inbound,
                        ClientError::ObservationQueue,
                        Inbound::Observation(ObservationMessage {
                            message_type: message.message_type,
                            subtype: message.subtype,
                            raw: message.raw,
                            value,
                        }),
                    ),
                    Err(error) => {
                        self.close_with(ClientError::Transport(error.to_string()));
                        true
                    }
                }
            }
            Kind::ControlRequest => {
                let value = match decode_control_request(&message.subtype, &message.raw) {
                    Ok(value) => value,
                    Err(error) => {
                        self.close_with(ClientError::Transport(error.to_string()));
                        return true;
                    }
                };
                let control = Arc::new(IncomingControl {
                    id: message.request_id,
                    subtype: message.subtype,
                    raw: message.raw,
                    value,
                    client: Arc::downgrade(&self.0),
                    done: Mutex::new(false),
                });
                let duplicate = {
                    let mut state = lock(&self.0.state);
                    let duplicate = state.incoming.contains_key(&control.id);
                    if !duplicate && !state.closed {
                        state.incoming.insert(control.id.clone(), control.clone());
                    }
                    duplicate
                };
                if duplicate {
                    self.close_with(ClientError::DuplicateRequestId(control.id.clone()));
                    return true;
                }
                !self.enqueue(
                    inbound,
                    ClientError::ControlQueue,
                    Inbound::Control(control),
                )
            }
            Kind::ControlCancel => {
                let retired = lock(&self.0.state).incoming.remove(&message.request_id);
                if let Some(control) = retired {
                    *lock(&control.done) = true;
                }
                let notice = CancelNotice {
                    request_id: message.request_id,
                };
                !self.enqueue(inbound, ClientError::ControlQueue, Inbound::Cancel(notice))
            }
        }
    }

    fn enqueue(&self, inbound: &Sender<Inbound>, overflow: ClientError, message: Inbound) -> bool {
        let message = match inbound.try_send(message) {
            Ok(()) => return true,
            Err(TrySendError::Full(message)) => message,
            Err(TrySendError::Disconnected(_)) => return false,
        };
        select! {
            send(inbound, message) -> sent => sent.is_ok(),
            recv(self.0.done_rx) -> _ => false,
            recv(after(self.0.stall)) -> _ => {
                self.close_with(overflow);
                false
            }
        }
    }

    fn barrier(&self, inbound: &Sender<Inbound>) -> bool {
        let (ack, acked) = crossbeam_channel::bounded::<()>(0);
        if !self.enqueue(
            inbound,
            ClientError::ObservationQueue,
            Inbound::Barrier(ack),
        ) {
            return false;
        }
        select! {
            recv(acked) -> _ => {}
            recv(self.0.done_rx) -> _ => {}
        }
        true
    }
}

#[cfg(test)]
#[path = "../../tests/support/rpc_ownership.rs"]
mod ownership;
