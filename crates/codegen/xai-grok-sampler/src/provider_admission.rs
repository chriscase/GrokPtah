//! Admit a sampler HTTP request through the host-authority lattice.
//!
//! This module is not a second transport: it builds a [`RequestIdentity`],
//! calls [`OperatorSendHost::admit`] (which ends in `admit_sending`), then
//! performs the single `Client::execute`. Settlement consumes the permit.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::Stream;
use reqwest::header::{AUTHORIZATION, HeaderMap};
use xai_grok_sampling_types::SamplingError;
use xai_host_authority::{
    AuthorityError, FailedReason, OperatorSendHost, PhysicalSendPermit, RequestIdentity,
    UncertainReason,
};

pub(crate) struct AdmittedResponse {
    response: reqwest::Response,
    settlement: Arc<WireSettlement>,
}

struct WireSettlement {
    host: Arc<OperatorSendHost>,
    permit: Mutex<Option<PhysicalSendPermit>>,
}

impl WireSettlement {
    fn complete(&self) {
        if let Some(permit) = take_permit(&self.permit) {
            let _ = self.host.settle_settled(permit);
        }
    }

    fn fail_before_write(&self, reason: FailedReason) {
        if let Some(permit) = take_permit(&self.permit) {
            let _ = self.host.settle_failed_before_write(permit, reason);
        }
    }

    fn fail_uncertain(&self, reason: UncertainReason) {
        if let Some(permit) = take_permit(&self.permit) {
            let _ = self.host.settle_uncertain(permit, reason);
        }
    }
}

impl Drop for WireSettlement {
    fn drop(&mut self) {
        self.fail_uncertain(UncertainReason::ResponseBodyAfterPossibleEffect);
    }
}

fn take_permit(permit: &Mutex<Option<PhysicalSendPermit>>) -> Option<PhysicalSendPermit> {
    permit
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

pub(crate) fn is_retry_oriented_http_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status.as_u16() == 429
        || status.is_server_error()
}

impl AdmittedResponse {
    pub(crate) fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }

    pub(crate) fn headers(&self) -> &HeaderMap {
        self.response.headers()
    }

    pub(crate) async fn bytes(self) -> Result<Bytes, SamplingError> {
        let status = self.response.status();
        let settlement = self.settlement;
        match self.response.bytes().await {
            Ok(bytes) => {
                if is_retry_oriented_http_status(status) {
                    settlement.fail_uncertain(UncertainReason::ProtocolAfterPossibleEffect);
                } else {
                    settlement.complete();
                }
                Ok(bytes)
            }
            Err(error) => {
                settlement.fail_uncertain(UncertainReason::ResponseBodyAfterPossibleEffect);
                Err(SamplingError::Http(error))
            }
        }
    }

    pub(crate) async fn text(self) -> Result<String, SamplingError> {
        let bytes = self.bytes().await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub(crate) fn into_byte_stream(
        self,
    ) -> (
        impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
        StreamSettlement,
    ) {
        let settlement = StreamSettlement(Arc::clone(&self.settlement));
        let stream = self.response.bytes_stream();
        (stream, settlement)
    }
}

pub(crate) async fn send_admitted(
    client: &reqwest::Client,
    builder: reqwest::RequestBuilder,
    dialect: &'static str,
    model: &str,
    target_scope: &'static str,
) -> Result<AdmittedResponse, SamplingError> {
    let request = builder.build().map_err(SamplingError::Http)?;
    let body = match request.body() {
        None => &[][..],
        Some(body) => body.as_bytes().ok_or(SamplingError::InvalidConfiguration(
            "provider request body is not immutable bytes",
        ))?,
    };
    let credential = credential_bytes(&request);
    let identity = RequestIdentity::new_with_provider_request_id(
        request.url().as_str(),
        request.method().as_str(),
        dialect,
        &credential,
        model,
        request
            .headers()
            .get("x-grok-req-id")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default(),
        body,
    );
    let host = OperatorSendHost::process().map_err(map_authority_error)?;
    let (_auth, permit) = host
        .admit(&identity, target_scope)
        .map_err(map_authority_error)?;
    let settlement = Arc::new(WireSettlement {
        host: Arc::clone(&host),
        permit: Mutex::new(Some(permit)),
    });
    match client.execute(request).await {
        Ok(response) => Ok(AdmittedResponse {
            response,
            settlement,
        }),
        Err(error) if error.is_connect() => {
            settlement.fail_before_write(FailedReason::ConnectRefusedBeforeWrite);
            Err(SamplingError::Http(error))
        }
        Err(error) => {
            settlement.fail_uncertain(UncertainReason::TransportAfterPossibleWrite);
            Err(SamplingError::Http(error))
        }
    }
}

fn credential_bytes(request: &reqwest::Request) -> Vec<u8> {
    if let Some(value) = request.headers().get(AUTHORIZATION) {
        return value.as_bytes().to_vec();
    }
    if let Some(value) = request.headers().get("x-api-key") {
        return value.as_bytes().to_vec();
    }
    Vec::new()
}

fn map_authority_error(error: AuthorityError) -> SamplingError {
    SamplingError::StreamError {
        error_type: "provider_send_denied".into(),
        message: error.to_string(),
    }
}

#[derive(Clone)]
pub(crate) struct StreamSettlement(Arc<WireSettlement>);

impl StreamSettlement {
    pub(crate) fn complete(&self) {
        self.0.complete();
    }

    pub(crate) fn fail(&self) {
        self.0
            .fail_uncertain(UncertainReason::ResponseBodyAfterPossibleEffect);
    }
}
