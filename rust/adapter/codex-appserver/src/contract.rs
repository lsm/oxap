use std::sync::Arc;

use oxap_adapter::{
    Adapter as AdapterContract, BoxFuture, Descriptor, Error, EventStream, InteractionResolution,
    OpenRequest, Recovery, ResumeRequest, Session as SessionContract, SettingsUpdater,
    SubmitRequest,
};
use oxap_protocol::{
    MessageSubmitResponse, RunCancelResponse, RunId, SessionSettingsUpdateRequest,
    SessionSettingsUpdateResponse, SessionState,
};

use crate::adapter::Adapter;
use crate::caller::Caller;
use crate::session::Session;

struct Leave(Arc<Caller>);

impl Drop for Leave {
    fn drop(&mut self) {
        self.0.leave();
    }
}

fn background<T: Send + 'static>(
    work: impl FnOnce(&Caller) -> Result<T, Error> + Send + 'static,
) -> BoxFuture<'static, T> {
    Box::pin(async move {
        let caller = Arc::new(Caller::default());
        let leave = Leave(caller.clone());
        let (reply, replied) = futures_channel::oneshot::channel();
        std::thread::spawn(move || {
            let _ = reply.send(work(&caller));
        });
        let result = replied.await.unwrap_or(Err(Error::SessionClosed));
        drop(leave);
        result
    })
}

impl AdapterContract for Adapter {
    fn probe(&self) -> BoxFuture<'_, Descriptor> {
        let descriptor = self.describe();
        Box::pin(async move { Ok(descriptor) })
    }

    fn open(&self, request: OpenRequest) -> BoxFuture<'_, Box<dyn SessionContract>> {
        let adapter = self.clone();
        background(move |caller| {
            adapter
                .open_as(request, caller)
                .map(|session| Box::new(session) as Box<dyn SessionContract>)
        })
    }
}

impl SessionContract for Session {
    fn submit(
        &self,
        request: SubmitRequest,
    ) -> BoxFuture<'_, (MessageSubmitResponse, EventStream)> {
        let shared = self.shared.clone();
        background(move |caller| shared.submit(request.request, caller))
    }

    fn state(&self) -> BoxFuture<'_, SessionState> {
        let shared = self.shared.clone();
        background(move |_| shared.state())
    }

    fn resolve(&self, resolution: InteractionResolution) -> BoxFuture<'_, ()> {
        let shared = self.shared.clone();
        background(move |caller| shared.resolve(resolution, caller))
    }

    fn cancel(&self, run_id: RunId) -> BoxFuture<'_, RunCancelResponse> {
        let shared = self.shared.clone();
        background(move |caller| shared.cancel(&run_id, caller))
    }

    fn resume(&self, request: ResumeRequest) -> BoxFuture<'_, (Recovery, EventStream)> {
        let shared = self.shared.clone();
        background(move |_| shared.resume(request))
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        let shared = self.shared.clone();
        background(move |_| shared.close())
    }

    fn native_session_id(&self) -> Option<String> {
        Some(self.shared.native_session_id())
    }

    fn settings_updater(&self) -> Option<&dyn SettingsUpdater> {
        Some(self)
    }
}

impl SettingsUpdater for Session {
    fn update_settings(
        &self,
        request: SessionSettingsUpdateRequest,
    ) -> BoxFuture<'_, (SessionSettingsUpdateResponse, SessionState)> {
        let shared = self.shared.clone();
        background(move |_| shared.update_settings(request))
    }
}
