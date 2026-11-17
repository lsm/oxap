use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender};
use oxap_adapter::Error;

use crate::client::Client;
use crate::rpc::{self, error, Fault};
use crate::session::hold;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Waiting,
    Calling,
    Gone,
}

struct State {
    phase: Phase,
    client: Option<Arc<dyn Client>>,
    cancel: Option<Sender<()>>,
}

pub(crate) struct Caller {
    state: Mutex<State>,
    cancelled: Receiver<()>,
}

impl Default for Caller {
    fn default() -> Self {
        let (cancel, cancelled) = crossbeam_channel::bounded(0);
        Self {
            state: Mutex::new(State {
                phase: Phase::Waiting,
                client: None,
                cancel: Some(cancel),
            }),
            cancelled,
        }
    }
}

impl Caller {
    pub(crate) fn cancelled(&self) -> &Receiver<()> {
        &self.cancelled
    }

    pub(crate) fn here(&self) -> Result<(), Error> {
        match hold(&self.state).phase {
            Phase::Gone => Err(Error::SessionClosed),
            _ => Ok(()),
        }
    }

    pub(crate) fn call<T>(
        &self,
        client: &Arc<dyn Client>,
        work: impl FnOnce() -> Result<T, rpc::Error>,
    ) -> Result<T, rpc::Error> {
        {
            let mut state = hold(&self.state);
            if state.phase == Phase::Gone {
                return Err(error(Fault::Closed));
            }
            state.phase = Phase::Calling;
            state.client = Some(client.clone());
        }
        let result = work();
        let mut state = hold(&self.state);
        if state.phase == Phase::Calling {
            state.phase = Phase::Waiting;
        }
        result
    }

    pub(crate) fn leave(&self) {
        let mut state = hold(&self.state);
        if let (Phase::Calling, Some(client)) = (state.phase, &state.client) {
            client.abort();
        }
        state.phase = Phase::Gone;
        state.cancel = None;
    }
}
