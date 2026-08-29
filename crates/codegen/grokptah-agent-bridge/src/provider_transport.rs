//! The only credential-bearing provider wire-emission boundary.
//!
//! Callers construct a complete `reqwest::Request`, but only this module may
//! hand it to the HTTP client. Immediately before that handoff it obtains a
//! one-use [`PhysicalSendPermit`] from the canonical host-authority root. The
//! permit is then consumed by exactly one terminal settlement.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use xai_host_authority::{
    ActorClass, ContentDigest, EffectClass, FailedReason, HostAdminCredential, HostAuthority,
    HostCredential, PhysicalSendPermit, RequestIdentity, SendOutcome, UncertainReason,
};

const AUTHORITY_DIR: &str = "authority/provider-send-v1";
const CUSTODY_FILE: &str = "authority/provider-send-v1.key";
const SERVICE_CREDENTIAL_ID: &str = "provider-transport";
const CAPABILITY_TTL_MS: u64 = 60_000;
const LEASE_TTL_MS: u64 = 30_000;

static AUTHORITIES: OnceLock<Mutex<HashMap<PathBuf, Arc<ProviderAuthority>>>> = OnceLock::new();

struct ProviderAuthority {
    authority: HostAuthority,
    service_bearer: String,
}

/// Secret-bearing material used only to bind the permit to the exact wire
/// request. It is digested synchronously and is never retained by authority.
pub(crate) struct ProviderRequestScope<'a> {
    pub credential_secret: &'a [u8],
    pub dialect: &'a str,
    pub model: &'a str,
    /// Secret-free logical surface (for example `agent-step` or
    /// `provider-qualification`). This participates in the resource binding.
    pub target_scope: &'a str,
}

#[derive(Debug)]
pub(crate) struct ProviderTransportError {
    message: String,
    outcome: Option<SendOutcome>,
}

impl ProviderTransportError {
    fn before_dispatch(error: impl std::fmt::Display) -> Self {
        Self {
            message: format!("provider send authority refused before dispatch: {error}"),
            outcome: None,
        }
    }

    fn settled(message: impl Into<String>, outcome: SendOutcome) -> Self {
        Self {
            message: message.into(),
            outcome: Some(outcome),
        }
    }

    /// Only a proven connect-time refusal can be retried automatically. An
    /// absent outcome is an authority denial and also cannot be retried by a
    /// lower layer: policy must explicitly grant a new attempt.
    pub(crate) fn is_safe_to_resend(&self) -> bool {
        self.outcome
            .as_ref()
            .is_some_and(SendOutcome::is_safe_to_resend)
    }

    pub(crate) fn is_uncertain(&self) -> bool {
        self.outcome
            .as_ref()
            .is_some_and(|outcome| matches!(outcome, SendOutcome::Uncertain { .. }))
    }
}

impl std::fmt::Display for ProviderTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderTransportError {}

enum WireResult {
    Response(reqwest::Response),
    RefusedBeforeWrite(String),
    Ambiguous(String),
}

#[async_trait]
trait WireDispatch: Send + Sync {
    async fn dispatch(
        &self,
        client: &reqwest::Client,
        request: reqwest::Request,
        permit: &PhysicalSendPermit,
    ) -> WireResult;
}

struct ReqwestWireDispatch;

#[async_trait]
impl WireDispatch for ReqwestWireDispatch {
    async fn dispatch(
        &self,
        client: &reqwest::Client,
        request: reqwest::Request,
        _permit: &PhysicalSendPermit,
    ) -> WireResult {
        match client.execute(request).await {
            Ok(response) => WireResult::Response(response),
            Err(error) if error.is_connect() => {
                WireResult::RefusedBeforeWrite("connection refused before request write".into())
            }
            Err(error) => WireResult::Ambiguous(if error.is_timeout() {
                "provider request timed out after dispatch began".into()
            } else {
                "provider transport failed after dispatch began".into()
            }),
        }
    }
}

/// Execute one credential-bearing request through the canonical physical-send
/// lattice. The response is returned only if the `Settled` audit record is
/// durable. Thus no caller can observe a response and silently bypass a failed
/// settlement write.
pub(crate) async fn send_provider_request(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    scope: ProviderRequestScope<'_>,
    cancel: Option<&CancellationToken>,
) -> Result<reqwest::Response, ProviderTransportError> {
    send_provider_request_with(client, request, scope, cancel, &ReqwestWireDispatch).await
}

async fn send_provider_request_with(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    scope: ProviderRequestScope<'_>,
    cancel: Option<&CancellationToken>,
    dispatch: &dyn WireDispatch,
) -> Result<reqwest::Response, ProviderTransportError> {
    let request = request
        .build()
        .map_err(ProviderTransportError::before_dispatch)?;
    let body = request
        .body()
        .and_then(reqwest::Body::as_bytes)
        .unwrap_or_default();
    let identity = RequestIdentity::new(
        request.url().as_str(),
        request.method().as_str(),
        scope.dialect,
        scope.credential_secret,
        scope.model,
        body,
    );
    let runtime = authority_runtime().map_err(ProviderTransportError::before_dispatch)?;
    let permit = runtime
        .permit(&identity, scope.target_scope)
        .map_err(ProviderTransportError::before_dispatch)?;

    let result = if let Some(cancel) = cancel {
        tokio::select! {
            result = dispatch.dispatch(client, request, &permit) => result,
            _ = cancel.cancelled() => {
                let outcome = runtime.authority.settle_uncertain(
                    permit,
                    UncertainReason::CancelledAfterPossibleWrite,
                );
                return Err(ProviderTransportError::settled(
                    "provider request cancelled after dispatch began; outcome is uncertain",
                    outcome,
                ));
            }
        }
    } else {
        dispatch.dispatch(client, request, &permit).await
    };

    match result {
        WireResult::Response(response) => {
            let outcome = runtime.authority.settle_settled(permit);
            if matches!(outcome, SendOutcome::Settled { .. }) {
                Ok(response)
            } else {
                Err(ProviderTransportError::settled(
                    "provider response arrived but settlement is not durable; outcome is uncertain",
                    outcome,
                ))
            }
        }
        WireResult::RefusedBeforeWrite(message) => {
            let outcome = runtime
                .authority
                .settle_failed_before_write(permit, FailedReason::ConnectRefusedBeforeWrite);
            Err(ProviderTransportError::settled(message, outcome))
        }
        WireResult::Ambiguous(message) => {
            let outcome = runtime
                .authority
                .settle_uncertain(permit, UncertainReason::TransportAfterPossibleWrite);
            Err(ProviderTransportError::settled(message, outcome))
        }
    }
}

impl ProviderAuthority {
    fn permit(
        &self,
        request: &RequestIdentity,
        target_scope: &str,
    ) -> anyhow::Result<PhysicalSendPermit> {
        let auth = self.authority.authenticate(&self.service_bearer)?;
        let session = self.authority.issue_session(&auth)?;
        let cwd = std::env::current_dir().context("resolve provider-send workspace")?;
        let workspace = self.authority.issue_workspace(&auth, &cwd)?;
        let observation =
            ContentDigest::of_fields(&[("provider-send-target-v1", target_scope.as_bytes())]);
        let resource = self
            .authority
            .issue_resource(&auth, session, workspace, observation)?;
        let capability = self.authority.seal_capability(
            &auth,
            resource,
            ActorClass::VerifiedModel,
            EffectClass::ProviderSend,
            CAPABILITY_TTL_MS,
        )?;
        let lease =
            self.authority
                .mint_lease(&auth, &capability, request.digest(), LEASE_TTL_MS)?;
        Ok(self.authority.begin_send(&auth, lease, request)?)
    }
}

fn authority_runtime() -> anyhow::Result<Arc<ProviderAuthority>> {
    let home = crate::discover::grokptah_home();
    let root = home.join(AUTHORITY_DIR);
    let registry = AUTHORITIES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .map_err(|_| anyhow!("provider authority registry poisoned"))?;
    if let Some(runtime) = registry.get(&root) {
        return Ok(Arc::clone(runtime));
    }

    fs::create_dir_all(&home).context("create GrokPtah home for provider authority")?;
    fs::create_dir_all(&root).context("create provider authority root")?;
    secure_authority_directory(&home.join("authority"))?;
    secure_authority_directory(&root)?;
    let custody = load_or_create_custody(&home.join(CUSTODY_FILE))?;
    let admin = HostAdminCredential::new(custody.clone())?;
    let (authority, admin_authority) = HostAuthority::open(&root, &admin)?;
    let service_bearer = derive_service_bearer(&custody);
    authority.set_credentials(
        &admin_authority,
        &[HostCredential::new(
            SERVICE_CREDENTIAL_ID,
            service_bearer.clone(),
        )?],
    )?;
    let runtime = Arc::new(ProviderAuthority {
        authority,
        service_bearer,
    });
    registry.insert(root, Arc::clone(&runtime));
    Ok(runtime)
}

fn secure_authority_directory(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn derive_service_bearer(custody: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"grokptah-provider-transport-service-v1\0");
    digest.update(custody.as_bytes());
    format!("{:x}", digest.finalize())
}

fn load_or_create_custody(path: &Path) -> anyhow::Result<String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match create_custody(path) {
        Ok(secret) => Ok(secret),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read_custody(path),
        Err(error) => Err(error).context("create provider authority custody key"),
    }
}

fn create_custody(path: &Path) -> std::io::Result<String> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let secret = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    file.write_all(secret.as_bytes())?;
    file.sync_all()?;
    Ok(secret)
}

fn read_custody(path: &Path) -> anyhow::Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = fs::metadata(path)?.mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(anyhow!(
                "provider authority custody key must not be group/world accessible"
            ));
        }
    }
    let mut value = String::new();
    OpenOptions::new()
        .read(true)
        .open(path)?
        .take(4096)
        .read_to_string(&mut value)?;
    let value = value.trim().to_string();
    if value.len() < 32 {
        return Err(anyhow!("provider authority custody key is incomplete"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct HomeOverride;

    impl HomeOverride {
        fn install(path: PathBuf) -> Self {
            crate::discover::set_grokptah_home_override(Some(path));
            Self
        }
    }

    impl Drop for HomeOverride {
        fn drop(&mut self) {
            crate::discover::set_grokptah_home_override(None);
        }
    }

    struct InjectedDispatch {
        calls: AtomicUsize,
        result: Mutex<Option<WireResult>>,
    }

    struct PendingDispatch;

    #[async_trait]
    impl WireDispatch for PendingDispatch {
        async fn dispatch(
            &self,
            _client: &reqwest::Client,
            _request: reqwest::Request,
            _permit: &PhysicalSendPermit,
        ) -> WireResult {
            std::future::pending::<WireResult>().await
        }
    }

    #[async_trait]
    impl WireDispatch for InjectedDispatch {
        async fn dispatch(
            &self,
            _client: &reqwest::Client,
            _request: reqwest::Request,
            _permit: &PhysicalSendPermit,
        ) -> WireResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.lock().unwrap().take().unwrap()
        }
    }

    fn scope() -> ProviderRequestScope<'static> {
        ProviderRequestScope {
            credential_secret: b"synthetic-provider-secret",
            dialect: "test",
            model: "synthetic-model",
            target_scope: "test-provider-send",
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn injected_prewrite_refusal_is_the_only_retryable_failure() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let dispatch = InjectedDispatch {
            calls: AtomicUsize::new(0),
            result: Mutex::new(Some(WireResult::RefusedBeforeWrite("refused".into()))),
        };
        let client = reqwest::Client::new();
        let error = send_provider_request_with(
            &client,
            client.post("http://127.0.0.1/provider").body("body"),
            scope(),
            None,
            &dispatch,
        )
        .await
        .unwrap_err();
        assert!(error.is_safe_to_resend());
        assert!(!error.is_uncertain());
        assert_eq!(dispatch.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn injected_postwrite_failure_is_uncertain_and_never_retryable() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let dispatch = InjectedDispatch {
            calls: AtomicUsize::new(0),
            result: Mutex::new(Some(WireResult::Ambiguous("cut after write".into()))),
        };
        let client = reqwest::Client::new();
        let error = send_provider_request_with(
            &client,
            client.post("http://127.0.0.1/provider").body("body"),
            scope(),
            None,
            &dispatch,
        )
        .await
        .unwrap_err();
        assert!(error.is_uncertain());
        assert!(!error.is_safe_to_resend());
        assert_eq!(dispatch.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn cancellation_after_permit_is_uncertain_and_never_retryable() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let client = reqwest::Client::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = send_provider_request_with(
            &client,
            client.post("http://127.0.0.1/provider").body("body"),
            scope(),
            Some(&cancel),
            &PendingDispatch,
        )
        .await
        .unwrap_err();
        assert!(error.is_uncertain());
        assert!(!error.is_safe_to_resend());
    }

    #[test]
    fn credential_bearing_model_calls_have_no_raw_send_escape_hatch() {
        let host_helpers = include_str!("host_helpers.rs");
        let qualification = include_str!("provider_qualification.rs");
        let discovery = include_str!("provider_discovery.rs");
        let auth_store = include_str!("auth_store.rs");
        let raw_send = [".", "send()"].concat();
        // The one remaining host_helpers call is the unauthenticated web-fetch
        // tool; the one auth-store call is OIDC discovery before any refresh
        // credential is attached. Every provider/model or secret-bearing call
        // is forced through this module.
        assert_eq!(host_helpers.matches(&raw_send).count(), 1);
        assert_eq!(qualification.matches(&raw_send).count(), 0);
        assert_eq!(discovery.matches(&raw_send).count(), 0);
        assert_eq!(auth_store.matches(&raw_send).count(), 1);
        let this_module = include_str!("provider_transport.rs");
        let execute_needle = ["client.execute", "(request)"].concat();
        assert_eq!(this_module.matches(&execute_needle).count(), 1);
    }
}
