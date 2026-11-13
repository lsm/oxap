use oxap_adapter::{refuse_unadvertised_live_settings, Error};
use oxap_protocol::{
    CapabilityDescriptor, SessionSettingsUpdateRequest, SessionSettingsUpdateResponse, SessionState,
};

use crate::adapter::{effort, features};
use crate::session::Shared;

impl Shared {
    pub(crate) fn update_settings(
        &self,
        request: SessionSettingsUpdateRequest,
    ) -> Result<(SessionSettingsUpdateResponse, SessionState), Error> {
        let descriptor = CapabilityDescriptor {
            features: features(),
            ..CapabilityDescriptor::default()
        };
        refuse_unadvertised_live_settings(&request, &descriptor)?;
        let _operating = self.operate();
        let mut inner = self.lock();
        if inner.closed {
            return Err(Error::SessionClosed);
        }
        if request.session_id != inner.state.session_id {
            return Err(Error::RunNotFound);
        }
        if inner.active.is_some() {
            return Err(Error::RunActive);
        }
        let mut response = SessionSettingsUpdateResponse {
            session_id: inner.state.session_id.clone(),
            ..SessionSettingsUpdateResponse::default()
        };
        if !request.reasoning_level.is_empty() {
            inner.effort = effort(&request.reasoning_level).to_owned();
            response.previous_reasoning_level = inner.state.reasoning_level.clone();
            inner.state.reasoning_level = request.reasoning_level.clone();
            response.reasoning_level = request.reasoning_level;
        }
        inner.state.updated_at_ms = self.runtime.now_ms();
        Ok((response, inner.state.clone()))
    }
}
