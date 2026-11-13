use std::sync::{Arc, Mutex};

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

pub(crate) struct Caller(Mutex<(Phase, Option<Arc<dyn Client>>)>);

impl Default for Caller {
    fn default() -> Self {
        Self(Mutex::new((Phase::Waiting, None)))
    }
}

impl Caller {
    pub(crate) fn here(&self) -> Result<(), Error> {
        match hold(&self.0).0 {
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
            let mut state = hold(&self.0);
            if state.0 == Phase::Gone {
                return Err(error(Fault::Closed));
            }
            *state = (Phase::Calling, Some(client.clone()));
        }
        let result = work();
        let mut state = hold(&self.0);
        if state.0 == Phase::Calling {
            state.0 = Phase::Waiting;
        }
        result
    }

    pub(crate) fn leave(&self) {
        let mut state = hold(&self.0);
        if let (Phase::Calling, Some(client)) = &*state {
            let _ = client.close();
        }
        state.0 = Phase::Gone;
    }
}
