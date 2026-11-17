use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::Receiver;
use serde_json::value::RawValue;

use crate::rpc::{Error, Inbound};

pub trait Client: Send + Sync {
    fn call(
        &self,
        method: &str,
        params: Box<RawValue>,
        deadline: Option<Instant>,
    ) -> Result<Box<RawValue>, Error>;

    fn inbound(&self) -> Receiver<Inbound>;

    fn done(&self) -> Receiver<()>;

    fn err(&self) -> Option<String>;

    fn close(&self) -> Result<(), Error>;

    fn abort(&self) {
        let _ = self.close();
    }
}

pub trait ClientFactory: Send + Sync {
    fn start(
        &self,
        deadline: Option<Instant>,
        cancel: &Receiver<()>,
    ) -> Result<Arc<dyn Client>, Error>;
}

impl<F> ClientFactory for F
where
    F: Fn(Option<Instant>, &Receiver<()>) -> Result<Arc<dyn Client>, Error> + Send + Sync,
{
    fn start(
        &self,
        deadline: Option<Instant>,
        cancel: &Receiver<()>,
    ) -> Result<Arc<dyn Client>, Error> {
        self(deadline, cancel)
    }
}
