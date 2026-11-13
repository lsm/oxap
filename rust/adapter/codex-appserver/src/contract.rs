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
use crate::session::Session;

fn background<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> BoxFuture<'static, T> {
    Box::pin(async move {
        let (reply, replied) = futures_channel::oneshot::channel();
        std::thread::spawn(move || {
            let _ = reply.send(work());
        });
        replied.await.unwrap_or(Err(Error::SessionClosed))
    })
}

impl AdapterContract for Adapter {
    fn probe(&self) -> BoxFuture<'_, Descriptor> {
        let descriptor = self.describe();
        Box::pin(async move { Ok(descriptor) })
    }

    fn open(&self, request: OpenRequest) -> BoxFuture<'_, Box<dyn SessionContract>> {
        let adapter = self.clone();
        background(move || {
            adapter
                .open(request)
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
        background(move || shared.submit(request.request))
    }

    fn state(&self) -> BoxFuture<'_, SessionState> {
        let state = self.shared.state();
        Box::pin(async move { state })
    }

    fn resolve(&self, resolution: InteractionResolution) -> BoxFuture<'_, ()> {
        let resolved = self.shared.resolve(resolution);
        Box::pin(async move { resolved })
    }

    fn cancel(&self, run_id: RunId) -> BoxFuture<'_, RunCancelResponse> {
        let shared = self.shared.clone();
        background(move || shared.cancel(&run_id))
    }

    fn resume(&self, request: ResumeRequest) -> BoxFuture<'_, (Recovery, EventStream)> {
        let resumed = self.shared.resume(request);
        Box::pin(async move { resumed })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        let shared = self.shared.clone();
        background(move || shared.close())
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
        let updated = self.shared.update_settings(request);
        Box::pin(async move { updated })
    }
}
