use std::time::Instant;

use oxap_adapter::{
    refuse_unadvertised_controls, BoxFuture, Error, EventStream, InteractionResolution, Recovery,
    ResumeRequest, Session as SessionContract, SubmitRequest, UnsupportedControl,
};
use oxap_protocol::{
    Admission, ContentPartType, EffectiveDeliveryMode, Message, MessageRole, MessageSubmitRequest,
    MessageSubmitResponse, RequestedDeliveryMode, RunCancelResponse, RunId, RunStatus,
    SessionState, FEATURE_DELIVERY_QUEUE,
};
use serde::Deserialize;

use super::{Inner, Session, Shared};
use crate::client;
use crate::native::{text, Event};

const FEATURE_RUN_CANCEL: &str = "run.cancel";
const INVALID_EVENT: &str = "the Copilot server sent an invalid session event";
const NO_MESSAGE_ID: &str = "copilot: session.send returned no messageId";

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Sent {
    message_id: String,
}

impl Session {
    pub fn submit(
        &self,
        mut request: MessageSubmitRequest,
    ) -> Result<(MessageSubmitResponse, EventStream), Error> {
        refuse_unadvertised_controls(&request, &[FEATURE_DELIVERY_QUEUE])?;
        if request.delivery.is_empty() {
            request.delivery = RequestedDeliveryMode::AUTO;
        }
        let shared = &self.shared;
        if request.session_id != *shared.lock().reducer.session_id() {
            return Err(Error::RunNotFound);
        }
        let delivery = request.delivery.clone();
        let queue = delivery == RequestedDeliveryMode::QUEUE;
        if request.target_run_id.is_some() || !(queue || delivery == RequestedDeliveryMode::AUTO) {
            return Err(Error::InvalidSubmission);
        }
        let prompt = text_prompt(&request.messages)?;
        let _sending = shared
            .sending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut inner = shared.lock();
        if inner.closed || inner.closing || inner.unusable {
            return Err(Error::SessionClosed);
        }
        let busy = inner.reducer.live() > 0;
        let index = inner.reducer.admit(queue)?;
        let mut follower = shared.journal.reserve();
        inner.pending = Some(index);
        let native = inner.reducer.native_id().to_owned();
        drop(inner);
        let params = serde_json::json!({ "sessionId": native, "prompt": prompt });
        let deadline = Instant::now() + shared.timeout;
        let sent = shared
            .process
            .client
            .call("session.send", &params, Some(deadline));
        let mut inner = shared.lock();
        inner.pending = None;
        let held = std::mem::take(&mut inner.held);
        let closed = inner.closed || inner.closing || inner.unusable;
        let reply = match sent {
            Ok(_) if closed => Err((Error::SessionClosed, false)),
            Ok(raw) => raw
                .and_then(|raw| serde_json::from_str::<Sent>(raw.get()).ok())
                .map(|sent| sent.message_id)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| (Error::Harness(NO_MESSAGE_ID.into()), false)),
            Err(error) => {
                let remote = matches!(error, client::Error::Remote { .. });
                Err((Error::harness(error), remote))
            }
        };
        let message_id = match reply {
            Ok(id) => id,
            Err((error, remote)) => {
                inner.reducer.runs.pop();
                follower.release();
                if !remote {
                    if !inner.closing {
                        let message = "Copilot prompt admission failed";
                        shared.end(&mut inner, "copilot_process_exit", message);
                    }
                    shared.process.client.close();
                }
                return Err(error);
            }
        };
        let run = &mut inner.reducer.runs[index];
        run.native_message = message_id;
        let (run_id, status, message) =
            (run.id.clone(), run.status.clone(), run.message_id.clone());
        let mut ack = MessageSubmitResponse {
            session_id: request.session_id.clone(),
            accepted: true,
            submission_id: inner.reducer.new_id("submission").into(),
            run_id: Some(run_id.clone()),
            status: status.clone(),
            requested_delivery: delivery,
            admission: Admission::STARTED,
            effective_delivery: EffectiveDeliveryMode::START,
            delivery_resolution: "session_idle".into(),
            message_ids: vec![message],
            ..MessageSubmitResponse::default()
        };
        if status == RunStatus::QUEUED {
            ack.admission = Admission::QUEUED;
            ack.effective_delivery = EffectiveDeliveryMode::QUEUE;
        }
        if busy {
            ack.delivery_resolution = "session_busy".into();
        }
        follower.bind(run_id.as_str());
        for event in held {
            if inner.reducer.observe(&event).is_err() {
                shared.end(&mut inner, "copilot_invalid_event", INVALID_EVENT);
                shared.process.client.close();
                break;
            }
        }
        shared.flush(&mut inner);
        Ok((ack, follower.stream()))
    }
}

impl Shared {
    pub(super) fn observe(&self, inner: &mut Inner, event: &Event) -> Result<(), Error> {
        let id = text(&event.data, "messageId");
        let unbound = event.kind == "user.message"
            && !id.is_empty()
            && !inner
                .reducer
                .runs
                .iter()
                .any(|run| run.native_message == id);
        let own = event.session_id == inner.reducer.native_id();
        if inner.pending.is_some() && (!inner.held.is_empty() || (own && unbound)) {
            if inner.held.len() >= self.held_limit {
                return Err(Error::harness(client::Error::Backpressure));
            }
            inner.held.push(event.clone());
            return Ok(());
        }
        inner.reducer.observe(event)
    }
}

fn text_prompt(messages: &[Message]) -> Result<String, Error> {
    let mut prompt = String::new();
    let mut text_bytes = 0;
    for (index, message) in messages.iter().enumerate() {
        if message.role != MessageRole::USER {
            return Err(Error::InvalidSubmission);
        }
        if index > 0 {
            prompt.push_str("\n\n");
        }
        if let Some(text) = message.content.text() {
            text_bytes += text.len();
            prompt.push_str(&text);
            continue;
        }
        let parts = message.content.parts().ok_or(Error::InvalidSubmission)?;
        for part in parts {
            if part.part_type != ContentPartType::TEXT {
                return Err(Error::InvalidSubmission);
            }
            text_bytes += part.text.len();
            prompt.push_str(&part.text);
        }
    }
    if text_bytes == 0 {
        return Err(Error::InvalidSubmission);
    }
    Ok(prompt)
}

impl SessionContract for Session {
    fn submit(
        &self,
        request: SubmitRequest,
    ) -> BoxFuture<'_, (MessageSubmitResponse, EventStream)> {
        Box::pin(async move { Session::submit(self, request.request) })
    }

    fn state(&self) -> BoxFuture<'_, SessionState> {
        Box::pin(async move { Session::state(self) })
    }

    fn resolve(&self, _: InteractionResolution) -> BoxFuture<'_, ()> {
        Box::pin(async move { Err(Error::InteractionNotFound) })
    }

    fn cancel(&self, _: RunId) -> BoxFuture<'_, RunCancelResponse> {
        let refusal = UnsupportedControl::unadvertised(FEATURE_RUN_CANCEL);
        Box::pin(async move { Err(Error::UnsupportedControl(Box::new(refusal))) })
    }

    fn resume(&self, request: ResumeRequest) -> BoxFuture<'_, (Recovery, EventStream)> {
        Box::pin(async move { Session::resume(self, &request) })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            Session::close(self);
            Ok(())
        })
    }

    fn native_session_id(&self) -> Option<String> {
        Some(Session::native_session_id(self))
    }
}
