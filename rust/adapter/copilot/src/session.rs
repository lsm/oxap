use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crossbeam_channel::{select, Receiver};
use oxap_adapter::journal::Journal;
use oxap_adapter::{Error, EventStream, Recovery, ResumeRequest};
use oxap_protocol::{ActiveRun, EnvelopeType, RunStatus, SessionState, SessionStatus};

use crate::native::{decode_event, Event, Message};
use crate::process::Process;
use crate::reducer::Reducer;

mod submit;

const RELATIONSHIP_PRIMARY: &str = "primary";

pub(crate) struct Inner {
    pub(crate) reducer: Reducer,
    pub(crate) closed: bool,
    pub(crate) closing: bool,
    pub(crate) unusable: bool,
    pub(crate) pending: Option<usize>,
    pub(crate) held: Vec<Event>,
}

pub(crate) struct Shared {
    pub(crate) inner: Mutex<Inner>,
    pub(crate) process: Process,
    pub(crate) journal: Journal,
    timeout: Duration,
    sending: Mutex<()>,
    held_limit: usize,
}

pub struct Session {
    pub(crate) shared: Arc<Shared>,
    done: Receiver<()>,
}

impl Session {
    pub fn new(reducer: Reducer, process: Process, capacity: usize, timeout: Duration) -> Self {
        let held_limit = process.client.notifications().capacity().unwrap_or(0);
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                reducer,
                closed: false,
                closing: false,
                unusable: false,
                pending: None,
                held: Vec::new(),
            }),
            process,
            journal: Journal::new(capacity),
            timeout,
            sending: Mutex::new(()),
            held_limit,
        });
        let (finished, done) = crossbeam_channel::bounded::<()>(0);
        let pump = shared.clone();
        std::thread::spawn(move || {
            pump.pump();
            drop(finished);
        });
        Self { shared, done }
    }

    pub fn native_session_id(&self) -> String {
        self.shared.lock().reducer.native_id().to_owned()
    }

    pub fn state(&self) -> Result<SessionState, Error> {
        let inner = self.shared.lock();
        if inner.closed {
            return Err(Error::SessionClosed);
        }
        Ok(inner.state())
    }

    pub fn resume(&self, request: &ResumeRequest) -> Result<(Recovery, EventStream), Error> {
        let inner = self.shared.lock();
        if inner.closed {
            return Err(Error::SessionClosed);
        }
        let pending = inner.pending;
        let Some(run) = inner
            .reducer
            .runs
            .iter()
            .enumerate()
            .find(|(index, run)| run.id == request.run_id && Some(*index) != pending)
            .map(|(_, run)| run)
        else {
            return Err(Error::RunNotFound);
        };
        let latest = run.next - 1;
        if request.after_sequence > latest {
            return Err(Error::ReplayCursorFuture);
        }
        let id = run.id.as_str();
        let journal = &self.shared.journal;
        let (recovery, replay) = journal.resume(inner.state(), id, request.after_sequence, latest);
        if let Some(gap) = recovery.replay_gap {
            return Err(Error::ReplayGap(gap));
        }
        Ok((recovery, replay))
    }

    pub fn close(&self) {
        self.shared.lock().closing = true;
        self.shared.process.close();
        let _ = self.done.recv();
        self.shared.lock().closed = true;
        self.shared.journal.close();
    }
}

impl Inner {
    pub(crate) fn state(&self) -> SessionState {
        let mut state = SessionState {
            session_id: self.reducer.session_id().clone(),
            status: SessionStatus::IDLE,
            ..SessionState::default()
        };
        let mut position = 0;
        let runs = self.reducer.runs.iter().enumerate();
        let open = runs.filter(|(index, run)| !run.terminal && Some(*index) != self.pending);
        for (_, run) in open {
            let mut active = ActiveRun {
                run_id: run.id.clone(),
                status: run.status.clone(),
                relationship: RELATIONSHIP_PRIMARY.into(),
                as_of_sequence: Some(run.next - 1),
                ..ActiveRun::default()
            };
            if run.status == RunStatus::QUEUED {
                position += 1;
                active.queue_position = Some(position);
            } else {
                state.active_run_id = Some(run.id.clone());
            }
            state.active_runs.push(active);
        }
        if state.active_run_id.is_some() {
            state.status = SessionStatus::RUNNING;
        } else if !state.active_runs.is_empty() {
            state.status = SessionStatus::QUEUED;
        }
        state
    }
}

impl Shared {
    pub(crate) fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn pump(&self) {
        let client = &self.process.client;
        let (notes, done) = (client.notifications(), client.done());
        loop {
            select! {
                recv(notes) -> message => match message {
                    Ok(message) => self.consume(message),
                    Err(_) => break,
                },
                recv(done) -> _ => break,
            }
        }
        self.process.close();
        while let Ok(message) = notes.try_recv() {
            self.consume(message);
        }
        let mut inner = self.lock();
        if inner.closing {
            self.end(&mut inner, "copilot_closed", "the Copilot session closed");
        } else {
            let message = "the Copilot server closed its RPC connection";
            self.end(&mut inner, "copilot_process_exit", message);
        }
    }

    fn consume(&self, message: Message) {
        let mut inner = self.lock();
        if inner.closed || inner.unusable {
            return;
        }
        let observed = match decode_event(&message) {
            Ok(Some(event)) => self.observe(&mut inner, &event).is_ok(),
            Ok(None) => true,
            Err(_) => false,
        };
        if !observed {
            let message = "the Copilot server sent an invalid session event";
            self.end(&mut inner, "copilot_invalid_event", message);
            self.process.client.close();
        }
        self.flush(&mut inner);
    }

    pub(crate) fn end(&self, inner: &mut Inner, code: &str, message: &str) {
        if inner.closed || inner.unusable {
            return;
        }
        inner.unusable = true;
        for index in 0..inner.reducer.runs.len() {
            if !inner.reducer.runs[index].terminal {
                let _ = inner.reducer.fail(index, code, message);
            }
        }
        self.flush(inner);
    }

    pub(crate) fn flush(&self, inner: &mut Inner) {
        for envelope in inner.reducer.events.drain(..) {
            let kind = &envelope.envelope_type;
            let terminal = *kind == EnvelopeType::RUN_COMPLETED
                || *kind == EnvelopeType::RUN_FAILED
                || *kind == EnvelopeType::RUN_CANCELLED;
            self.journal.append(envelope, terminal);
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
