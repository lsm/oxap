use std::{collections::BTreeMap, time::Instant};

use crossbeam_channel::select;
use oxap_adapter::{Error, NativeListing};
use serde::Deserialize;
use serde_json::value::{to_raw_value, RawValue};

use crate::process::{Process, ProcessConfig};

pub(crate) fn list(
    config: &ProcessConfig,
    limit: usize,
    deadline: Instant,
) -> Result<Vec<NativeListing>, Error> {
    let params = to_raw_value(&serde_json::json!({ "limit": limit })).map_err(Error::harness)?;
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
    let reply = process
        .client
        .call("session.list", Some(params), Some(deadline));
    let closed = process.close();
    let _ = drain.join();
    let reply = reply.map_err(Error::harness)?;
    closed.map_err(Error::harness)?;
    Ok(serde_json::from_str::<Page>(reply.get())
        .unwrap_or_default()
        .sessions
        .iter()
        .filter_map(|row| listing(row))
        .take(limit)
        .collect())
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
