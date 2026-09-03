//! In-process operator send host for CLI/TUI/ACP and embedding callers.
//!
//! Reuses the one [`crate::HostAuthority`] lattice. This is not a second
//! ledger, retry machine, or provider gateway: the only send authorisation
//! it produces is a [`crate::PhysicalSendPermit`] from
//! [`crate::HostAuthority::admit_operator_send`]. HTTP remains in the
//! caller. The process holds one root, opened from the existing grok home
//! (`GROK_HOME` or `~/.grok`) using the same `authority/provider-send-v1`
//! relative path as the desktop transport.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once, OnceLock};

use sha2::{Digest, Sha256};

use crate::digest::RequestIdentity;
use crate::error::AuthorityError;
use crate::receipt::{AuthContext, FailedReason, PhysicalSendPermit, SendOutcome, UncertainReason};
use crate::store::{HostAdminAuthority, HostAdminCredential, HostAuthority, HostCredential};

const AUTHORITY_REL: &str = "authority/provider-send-v1";
const CUSTODY_FILE: &str = "custody.key";
const OPERATOR_CREDENTIAL_ID: &str = "operator";

static ROOT_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);
static PROCESS_HOST: OnceLock<Result<Arc<OperatorSendHost>, AuthorityError>> = OnceLock::new();

/// Process-wide operator send host. Opaque: callers admit and settle; they
/// cannot mint a permit or skip [`crate::HostAuthority::admit_sending`].
pub struct OperatorSendHost {
    authority: HostAuthority,
    admin: HostAdminAuthority,
    operator_bearer: String,
}

impl std::fmt::Debug for OperatorSendHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OperatorSendHost([opaque])")
    }
}

/// Install the process operator-send root before the first admit.
///
/// Intended for tests that need an isolated tempdir. Not a send bypass:
/// every send still requires [`OperatorSendHost::admit`]. Has no effect
/// after [`OperatorSendHost::process`] has already opened a host.
pub fn install_operator_send_root(path: impl AsRef<Path>) {
    let mut guard = ROOT_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if PROCESS_HOST.get().is_none() {
        *guard = Some(path.as_ref().to_path_buf());
    }
}

impl OperatorSendHost {
    /// The process-wide operator send host, opened once.
    pub fn process() -> Result<Arc<Self>, AuthorityError> {
        isolate_cargo_target_binaries();
        match PROCESS_HOST.get_or_init(open_process_host) {
            Ok(host) => Ok(Arc::clone(host)),
            Err(error) => Err(error.clone()),
        }
    }

    /// Authenticate the operator, then begin and admit one send.
    ///
    /// No HTTP is performed here. The caller must dispatch immediately and
    /// settle the permit; dropping an admitted permit without settlement
    /// leaves the attempt `sending` until recovery classifies it Uncertain.
    pub fn admit(
        &self,
        request: &RequestIdentity,
        target_scope: &str,
    ) -> Result<(AuthContext, PhysicalSendPermit), AuthorityError> {
        let auth = self.authority.authenticate(&self.operator_bearer)?;
        let permit = self
            .authority
            .admit_operator_send(&auth, request, target_scope)?;
        Ok((auth, permit))
    }

    pub fn settle_settled(&self, permit: PhysicalSendPermit) -> SendOutcome {
        self.authority.settle_settled(permit)
    }

    pub fn settle_failed_before_write(
        &self,
        permit: PhysicalSendPermit,
        reason: FailedReason,
    ) -> SendOutcome {
        self.authority.settle_failed_before_write(permit, reason)
    }

    pub fn settle_uncertain(
        &self,
        permit: PhysicalSendPermit,
        reason: UncertainReason,
    ) -> SendOutcome {
        self.authority.settle_uncertain(permit, reason)
    }

    /// Test-only: advance capability generation so a previously prepared
    /// permit cannot be admitted. Not a send bypass.
    #[doc(hidden)]
    pub fn rotate_capability_generation(&self) -> Result<(), AuthorityError> {
        self.authority
            .rotate_capability_generation(&self.admin)
            .map(|_| ())
    }

    /// Test-only: replace the operator secret so the cached bearer is stale.
    #[doc(hidden)]
    pub fn replace_operator_secret(&self, secret: &str) -> Result<(), AuthorityError> {
        self.authority.set_credentials(
            &self.admin,
            &[HostCredential::new(OPERATOR_CREDENTIAL_ID, secret)?],
        )
    }

    #[doc(hidden)]
    pub fn attempt_states(&self) -> Result<Vec<String>, AuthorityError> {
        let auth = self.authority.authenticate(&self.operator_bearer)?;
        self.authority.read_attempt_states(&auth)
    }
}

impl HostAuthority {
    pub(crate) fn read_attempt_states(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<String>, AuthorityError> {
        self.read(|state| {
            crate::store::require_current_state(state, auth)?;
            Ok(state
                .attempts
                .values()
                .map(|record| record.state.clone())
                .collect())
        })
    }
}

fn isolate_cargo_target_binaries() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let mut guard = ROOT_OVERRIDE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_some() || PROCESS_HOST.get().is_some() {
            return;
        }
        if !exe_is_under_cargo_target() {
            return;
        }
        let path = std::env::temp_dir().join(format!(
            "xai-host-authority-operator-send-{}",
            std::process::id()
        ));
        let _ = fs::create_dir_all(&path);
        *guard = Some(path);
    });
}

fn exe_is_under_cargo_target() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let names: Vec<String> = exe
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    names.windows(2).any(|window| {
        window[0] == "target" && matches!(window[1].as_str(), "debug" | "release" | "deps" | "tmp")
    })
}

fn open_process_host() -> Result<Arc<OperatorSendHost>, AuthorityError> {
    let root = process_root()?;
    fs::create_dir_all(&root).map_err(|error| AuthorityError::Durability(error.to_string()))?;
    secure_directory(&root)?;
    let custody = load_or_create_custody(&root.join(CUSTODY_FILE))?;
    let admin = HostAdminCredential::new(custody.clone())?;
    let (authority, admin_authority) = HostAuthority::open(&root, &admin)?;
    authority.recover_incomplete(&admin_authority)?;
    let operator_bearer = derive_operator_bearer(&custody);
    authority.set_credentials(
        &admin_authority,
        &[HostCredential::new(
            OPERATOR_CREDENTIAL_ID,
            operator_bearer.clone(),
        )?],
    )?;
    Ok(Arc::new(OperatorSendHost {
        authority,
        admin: admin_authority,
        operator_bearer,
    }))
}

fn process_root() -> Result<PathBuf, AuthorityError> {
    if let Some(path) = ROOT_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return Ok(path);
    }
    Ok(grok_home().join(AUTHORITY_REL))
}

fn grok_home() -> PathBuf {
    if let Ok(home) = std::env::var("GROK_HOME") {
        return PathBuf::from(home);
    }
    #[allow(deprecated)]
    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".grok")
}

fn derive_operator_bearer(custody: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"xai-host-authority/operator-send-v1\0");
    digest.update(custody.as_bytes());
    format!("{:x}", digest.finalize())
}

fn secure_directory(path: &Path) -> Result<(), AuthorityError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| AuthorityError::Durability(error.to_string()))?;
    }
    let _ = path;
    Ok(())
}

fn load_or_create_custody(path: &Path) -> Result<String, AuthorityError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| AuthorityError::Durability(error.to_string()))?;
        secure_directory(parent)?;
    }
    match create_custody(path) {
        Ok(secret) => Ok(secret),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read_custody(path),
        Err(error) => Err(AuthorityError::Durability(error.to_string())),
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
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(secret)
}

fn read_custody(path: &Path) -> Result<String, AuthorityError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = fs::metadata(path)
            .map_err(|error| AuthorityError::Durability(error.to_string()))?
            .mode()
            & 0o777;
        if mode & 0o077 != 0 {
            return Err(AuthorityError::Durability(
                "operator send custody key must not be group/world accessible".into(),
            ));
        }
    }
    let mut value = String::new();
    OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|error| AuthorityError::Durability(error.to_string()))?
        .take(4096)
        .read_to_string(&mut value)
        .map_err(|error| AuthorityError::Durability(error.to_string()))?;
    let value = value.trim().to_string();
    if value.len() < 32 {
        return Err(AuthorityError::Durability(
            "operator send custody key is incomplete".into(),
        ));
    }
    Ok(value)
}
