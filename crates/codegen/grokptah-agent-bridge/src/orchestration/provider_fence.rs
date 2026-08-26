//! Physical provider-send fence.
//!
//! One durable provider-request identity is reused across safe retries.
//! Only [`ProviderSendState::KnownNotSent`] may auto-retry. A timeout, 408,
//! 429, 5xx, disconnect, or crash after the send begins is Uncertain unless
//! the provider contract proves non-delivery. Artifact bytes are streamed
//! through a local hasher; provider SHA values are never trusted on syntax.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::admission::DurableAdmission;
use super::authority::SpineError;
use super::lifecycle::ProviderSendState;

/// One physical coding-worker launch request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhysicalLaunchRequest {
    /// Stable provider-request identity, also sent as Idempotency-Key.
    pub provider_request_id: String,
    /// Opaque workspace identity.
    pub workspace_id: String,
    /// Immutable source revision.
    pub source_revision: String,
    /// Model class.
    pub model: String,
    /// Objective digest.
    pub objective_digest: String,
}

/// Physical launch acknowledgement.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhysicalLaunchAck {
    /// Provider-assigned run identity.
    pub provider_run_id: String,
    /// Echoed provider-request identity.
    pub provider_request_id: String,
}

/// Artifact listing item. Digest is a claim, not authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhysicalArtifactClaim {
    /// Artifact identity.
    pub artifact_id: String,
    /// Provider-claimed SHA-256 hex. Must be rehashed locally.
    pub claimed_sha256: String,
    /// Download path relative to the fake origin.
    pub path: String,
}

/// Host-computed artifact admission after streaming hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifact {
    /// Artifact identity.
    pub artifact_id: String,
    /// Local SHA-256 hex.
    pub digest_sha256: String,
    /// Admitted byte length.
    pub byte_len: u64,
}

/// In-process adversarial coding-worker. Not a live Grok Build endpoint.
#[derive(Clone)]
pub struct FakeCodingWorker {
    inner: Arc<Mutex<FakeInner>>,
    addr: SocketAddr,
    shutdown: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

struct FakeInner {
    launches: Vec<String>,
    /// If set, the next launch hangs until dropped (disconnect/timeout).
    hang_next: bool,
    /// If set, the next launch returns this status after recording the id.
    status_next: Option<u16>,
    /// Artifact bytes served for download.
    artifact_bytes: Vec<u8>,
    /// Claimed digest served in listings (may lie).
    claimed_digest: String,
}

impl FakeCodingWorker {
    /// Bind loopback and serve a documented-shaped coding-worker API.
    pub async fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind fake");
        let addr = listener.local_addr().expect("local addr");
        let inner = Arc::new(Mutex::new(FakeInner {
            launches: Vec::new(),
            hang_next: false,
            status_next: None,
            artifact_bytes: b"diff --git a/x b/x\n".to_vec(),
            claimed_digest: local_sha256(b"diff --git a/x b/x\n"),
        }));
        let (tx, rx) = oneshot::channel::<()>();
        let state = inner.clone();
        tokio::spawn(async move {
            let app = Router::new()
                .route("/v1/coding-runs", post(launch))
                .route("/v1/coding-runs/{id}/artifacts", get(list_artifacts))
                .route("/v1/artifacts/{id}", get(download_artifact))
                .with_state(state);
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await
                .ok();
        });
        Self {
            inner,
            addr,
            shutdown: Arc::new(Mutex::new(Some(tx))),
        }
    }

    /// Base URL.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    /// Recorded provider-request identities (physical launches).
    pub fn launches(&self) -> Vec<String> {
        self.inner.lock().launches.clone()
    }

    /// Next launch hangs (caller should treat as Uncertain).
    pub fn hang_next(&self) {
        self.inner.lock().hang_next = true;
    }

    /// Next launch returns an HTTP status after recording the identity.
    pub fn status_next(&self, status: u16) {
        self.inner.lock().status_next = Some(status);
    }

    /// Serve a lying digest for the listed artifact.
    pub fn lie_about_digest(&self, claimed: String) {
        self.inner.lock().claimed_digest = claimed;
    }

    /// Stop the server.
    pub fn stop(&self) {
        if let Some(tx) = self.shutdown.lock().take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for FakeCodingWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn launch(
    State(state): State<Arc<Mutex<FakeInner>>>,
    headers: HeaderMap,
    Json(body): Json<PhysicalLaunchRequest>,
) -> Result<Json<PhysicalLaunchAck>, StatusCode> {
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if key != body.provider_request_id {
        return Err(StatusCode::BAD_REQUEST);
    }
    let (hang, status) = {
        let mut inner = state.lock();
        if inner
            .launches
            .iter()
            .any(|id| id == &body.provider_request_id)
        {
            return Ok(Json(PhysicalLaunchAck {
                provider_run_id: format!("prun-{}", body.provider_request_id),
                provider_request_id: body.provider_request_id,
            }));
        }
        inner.launches.push(body.provider_request_id.clone());
        let hang = std::mem::replace(&mut inner.hang_next, false);
        let status = inner.status_next.take();
        (hang, status)
    };
    if hang {
        std::future::pending::<()>().await;
    }
    if let Some(code) = status {
        return Err(StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
    }
    Ok(Json(PhysicalLaunchAck {
        provider_run_id: format!("prun-{}", body.provider_request_id),
        provider_request_id: body.provider_request_id,
    }))
}

async fn list_artifacts(
    State(state): State<Arc<Mutex<FakeInner>>>,
    Path(id): Path<String>,
) -> Json<Vec<PhysicalArtifactClaim>> {
    let inner = state.lock();
    Json(vec![PhysicalArtifactClaim {
        artifact_id: format!("art-{id}"),
        claimed_sha256: inner.claimed_digest.clone(),
        path: format!("/v1/artifacts/{id}"),
    }])
}

async fn download_artifact(
    State(state): State<Arc<Mutex<FakeInner>>>,
    Path(_id): Path<String>,
) -> Response {
    let bytes = state.lock().artifact_bytes.clone();
    Response::builder()
        .status(StatusCode::OK)
        .header("content-length", bytes.len().to_string())
        .header("content-type", "application/octet-stream")
        .body(Body::from(bytes))
        .expect("artifact response")
}

fn local_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Dispatch one physical launch through the durable send lattice.
pub async fn physical_launch(
    admission: &DurableAdmission,
    base_url: &str,
    request: PhysicalLaunchRequest,
    timeout: Duration,
) -> Result<PhysicalLaunchAck, SpineError> {
    admission.auto_retry_allowed(&request.provider_request_id)?;
    let sending = admission.begin_send(&request.provider_request_id, 0);
    let sending = match sending {
        Ok(state) => state,
        Err(error) => return Err(error),
    };
    if sending != ProviderSendState::Sending {
        return Err(SpineError::TransitionForbidden);
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|_| SpineError::TransitionForbidden)?;
    let url = format!("{base_url}/v1/coding-runs");
    let response = client
        .post(&url)
        .header("Idempotency-Key", &request.provider_request_id)
        .json(&request)
        .send()
        .await;
    match response {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                let ack: PhysicalLaunchAck =
                    resp.json().await.map_err(|_| SpineError::MacInvalid)?;
                if ack.provider_request_id != request.provider_request_id {
                    admission
                        .mark_send_uncertain(
                            &request.provider_request_id,
                            1,
                            ProviderSendState::Sending,
                        )
                        .ok();
                    return Err(SpineError::CrossScope);
                }
                admission
                    .mark_sent(&request.provider_request_id, 1)
                    .map_err(|_| SpineError::TransitionForbidden)?;
                Ok(ack)
            } else if matches!(status.as_u16(), 408 | 429) || status.is_server_error() {
                admission.mark_send_uncertain(
                    &request.provider_request_id,
                    1,
                    ProviderSendState::Sending,
                )?;
                Err(SpineError::AutoRetryForbidden)
            } else if status.as_u16() == 400 || status.as_u16() == 401 || status.as_u16() == 403 {
                // Conclusive non-delivery: may return to KnownNotSent only if
                // the provider never recorded the identity. The fake records
                // first; treat as Uncertain to avoid a second paid mutation.
                admission.mark_send_uncertain(
                    &request.provider_request_id,
                    1,
                    ProviderSendState::Sending,
                )?;
                Err(SpineError::TransitionForbidden)
            } else {
                admission.mark_send_uncertain(
                    &request.provider_request_id,
                    1,
                    ProviderSendState::Sending,
                )?;
                Err(SpineError::TransitionForbidden)
            }
        }
        Err(_) => {
            admission.mark_send_uncertain(
                &request.provider_request_id,
                1,
                ProviderSendState::Sending,
            )?;
            Err(SpineError::AutoRetryForbidden)
        }
    }
}

/// Download an artifact and admit it only if the local digest matches.
pub async fn verify_artifact_bytes(
    base_url: &str,
    claim: &PhysicalArtifactClaim,
    max_bytes: u64,
) -> Result<VerifiedArtifact, SpineError> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| SpineError::TransitionForbidden)?;
    let url = format!("{base_url}{}", claim.path);
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|_| SpineError::TransitionForbidden)?;
    if !response.status().is_success() {
        return Err(SpineError::InvalidIdentity);
    }
    if let Some(len) = response.content_length() {
        if len > max_bytes {
            return Err(SpineError::Utf8Ceiling);
        }
    }
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut body = response.bytes_stream();
    use futures::StreamExt;
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|_| SpineError::TransitionForbidden)?;
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or(SpineError::Utf8Ceiling)?;
        if total > max_bytes {
            return Err(SpineError::Utf8Ceiling);
        }
        hasher.update(&chunk);
    }
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if hex != claim.claimed_sha256 {
        return Err(SpineError::MacInvalid);
    }
    Ok(VerifiedArtifact {
        artifact_id: claim.artifact_id.clone(),
        digest_sha256: hex,
        byte_len: total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::authority::{unsigned_provider_spec, LiveRevisions, MacKey};
    use crate::orchestration::spine_persist::SpinePersist;
    use crate::orchestration::DurableAdmission;

    fn key() -> MacKey {
        MacKey::from_bytes(&[0x19; 32]).unwrap()
    }

    async fn admitted(suffix: &str) -> (DurableAdmission, PhysicalLaunchRequest, FakeCodingWorker) {
        let dir = tempfile::tempdir().unwrap();
        let persist = SpinePersist::open(dir.path()).unwrap();
        // Keep persist alive by leaking the tempdir into the admission via the
        // path; tests finish quickly so the directory remains.
        let admission = DurableAdmission::new(persist);
        let spec = unsigned_provider_spec(suffix, "obj").seal(&key()).unwrap();
        let work = admission
            .admit(&key(), spec.clone(), LiveRevisions::default(), b"obj", 1)
            .unwrap();
        admission
            .persist_starting(&work.verified.spec().run_id, work.revision)
            .unwrap();
        let fake = FakeCodingWorker::spawn().await;
        let request = PhysicalLaunchRequest {
            provider_request_id: spec.provider_request_id,
            workspace_id: spec.workspace_id,
            source_revision: spec.workspace_source_revision,
            model: spec.model,
            objective_digest: spec.objective_digest,
        };
        std::mem::forget(dir);
        (admission, request, fake)
    }

    #[tokio::test]
    async fn successful_launch_reuses_identity_and_is_not_duplicated() {
        let (admission, request, fake) = admitted("ok").await;
        let ack = physical_launch(
            &admission,
            &fake.base_url(),
            request.clone(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(ack.provider_request_id, request.provider_request_id);
        assert_eq!(
            physical_launch(
                &admission,
                &fake.base_url(),
                request.clone(),
                Duration::from_secs(2),
            )
            .await
            .unwrap_err(),
            SpineError::AutoRetryForbidden
        );
        assert_eq!(fake.launches().len(), 1);
    }

    #[tokio::test]
    async fn timeout_after_send_is_uncertain_without_a_second_mutation() {
        let (admission, request, fake) = admitted("hang").await;
        fake.hang_next();
        let err = physical_launch(
            &admission,
            &fake.base_url(),
            request.clone(),
            Duration::from_millis(80),
        )
        .await
        .unwrap_err();
        assert_eq!(err, SpineError::AutoRetryForbidden);
        assert_eq!(fake.launches().len(), 1);
        assert_eq!(
            physical_launch(
                &admission,
                &fake.base_url(),
                request,
                Duration::from_millis(80),
            )
            .await
            .unwrap_err(),
            SpineError::AutoRetryForbidden
        );
        assert_eq!(fake.launches().len(), 1);
    }

    #[tokio::test]
    async fn artifact_digest_mismatch_is_rejected() {
        let fake = FakeCodingWorker::spawn().await;
        fake.lie_about_digest("00".repeat(32));
        let claim = PhysicalArtifactClaim {
            artifact_id: "art-1".into(),
            claimed_sha256: "00".repeat(32),
            path: "/v1/artifacts/art-1".into(),
        };
        let err = verify_artifact_bytes(&fake.base_url(), &claim, 1024)
            .await
            .unwrap_err();
        assert_eq!(err, SpineError::MacInvalid);
    }

    #[tokio::test]
    async fn artifact_bytes_are_locally_hashed_before_admission() {
        let fake = FakeCodingWorker::spawn().await;
        let claim = PhysicalArtifactClaim {
            artifact_id: "art-ok".into(),
            claimed_sha256: local_sha256(b"diff --git a/x b/x\n"),
            path: "/v1/artifacts/art-ok".into(),
        };
        let verified = verify_artifact_bytes(&fake.base_url(), &claim, 1024)
            .await
            .unwrap();
        assert_eq!(verified.digest_sha256, claim.claimed_sha256);
        assert!(verified.byte_len > 0);
    }
}
