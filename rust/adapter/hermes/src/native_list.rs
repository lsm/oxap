use std::{
    collections::BTreeMap,
    sync::{Mutex, TryLockError},
    thread::JoinHandle,
    time::Instant,
};

use crossbeam_channel::{select, TryRecvError};
use oxap_adapter::{Error, NativeListing};
use serde::Deserialize;
use serde_json::value::{to_raw_value, RawValue};

use crate::{
    process::{Process, ProcessConfig},
    rpc::{Error as RpcError, Fault},
};

#[derive(Default)]
pub(crate) struct ListingGateways {
    kept: Mutex<Option<ListingGateway>>,
}

impl ListingGateways {
    pub(crate) fn list(
        &self,
        config: &ProcessConfig,
        limit: usize,
        deadline: Instant,
    ) -> Result<Vec<NativeListing>, Error> {
        let params =
            to_raw_value(&serde_json::json!({ "limit": limit })).map_err(Error::harness)?;
        let reply = match self.kept.try_lock() {
            Ok(kept) => list_cached(kept, config, params, deadline),
            Err(TryLockError::Poisoned(error)) => {
                list_cached(error.into_inner(), config, params, deadline)
            }
            Err(TryLockError::WouldBlock) => {
                let mut gateway = ListingGateway::start(config, deadline)?;
                let reply = gateway.request(params, deadline);
                let closed = gateway.close();
                let reply = reply.map_err(Error::harness)?;
                closed?;
                Ok(reply)
            }
        }?;
        Ok(serde_json::from_str::<Page>(reply.get())
            .unwrap_or_default()
            .sessions
            .iter()
            .filter_map(|row| listing(row))
            .take(limit)
            .collect())
    }

    pub(crate) fn close(&self) -> Result<(), Error> {
        let mut kept = self.kept.lock().unwrap_or_else(|error| error.into_inner());
        kept.take().map_or(Ok(()), |mut gateway| gateway.close())
    }
}

fn list_cached(
    mut kept: std::sync::MutexGuard<'_, Option<ListingGateway>>,
    config: &ProcessConfig,
    params: Box<RawValue>,
    deadline: Instant,
) -> Result<Box<RawValue>, Error> {
    if kept.as_ref().is_some_and(ListingGateway::ended) {
        drop(kept.take());
    }
    let reused = kept.is_some();
    let reply = cached_request(&mut kept, config, params.clone(), deadline);
    if reused && kept.is_none() {
        cached_request(&mut kept, config, params, deadline)
    } else {
        reply
    }
}

fn cached_request(
    kept: &mut Option<ListingGateway>,
    config: &ProcessConfig,
    params: Box<RawValue>,
    deadline: Instant,
) -> Result<Box<RawValue>, Error> {
    if kept.is_none() {
        *kept = Some(ListingGateway::start(config, deadline)?);
    }
    let gateway = kept.as_ref().expect("listing gateway started");
    let reply = gateway.request(params, deadline);
    if reply
        .as_ref()
        .is_err_and(|error| !matches!(&error.fault, Fault::Remote { .. }) || gateway.ended())
    {
        drop(kept.take());
    }
    reply.map_err(Error::harness)
}

struct ListingGateway {
    process: Process,
    drain: Option<JoinHandle<()>>,
}

impl ListingGateway {
    fn start(config: &ProcessConfig, deadline: Instant) -> Result<Self, Error> {
        let process = Process::start(config, Some(deadline)).map_err(Error::harness)?;
        let inbound = process.client.inbound();
        let done = process.client.done();
        let drain = std::thread::Builder::new()
            .spawn(move || loop {
                select! {
                    recv(done) -> _ => break,
                    recv(inbound) -> message => if message.is_err() { break },
                }
            })
            .map_err(Error::harness)?;
        Ok(Self {
            process,
            drain: Some(drain),
        })
    }

    fn request(&self, params: Box<RawValue>, deadline: Instant) -> Result<Box<RawValue>, RpcError> {
        self.process
            .client
            .call("session.list", Some(params), Some(deadline))
    }

    fn ended(&self) -> bool {
        self.process.client.is_closed()
            || self.process.exited().try_recv() != Err(TryRecvError::Empty)
    }

    fn close(&mut self) -> Result<(), Error> {
        let closed = self.process.close().map_err(Error::harness);
        if let Some(drain) = self.drain.take() {
            let _ = drain.join();
        }
        closed
    }
}

impl Drop for ListingGateway {
    fn drop(&mut self) {
        if self.drain.is_some() {
            let _ = self.close();
        }
    }
}

#[derive(Default, Deserialize)]
struct Page {
    sessions: Vec<Box<RawValue>>,
}

fn listing(raw: &RawValue) -> Option<NativeListing> {
    let row: BTreeMap<String, &RawValue> = serde_json::from_str(raw.get()).ok()?;
    let text = |key| {
        row.get(key)
            .and_then(|value| serde_json::from_str::<String>(value.get()).ok())
            .unwrap_or_default()
    };
    let native_id = text("id");
    if native_id.is_empty() {
        return None;
    }
    let mut title = text("title");
    if title.is_empty() {
        title = text("preview");
    }
    let timestamp = row.get("started_at").map_or("", |value| value.get());
    let updated_at_ms = match timestamp.parse::<i64>() {
        Ok(seconds) => seconds.saturating_mul(1000),
        Err(_) => match timestamp.parse::<f64>() {
            Ok(seconds) if seconds.abs() < 1e15 => (seconds * 1000.0) as i64,
            _ => 0,
        },
    };
    Some(NativeListing {
        native_id,
        title,
        updated_at_ms,
        ..NativeListing::default()
    })
}
