//! Narrow internal surfaces that must not forge public principal authority.
//!
//! Readiness and health probes use [`InternalServiceAuthority`] so they can
//! report liveness without minting an [`AuthContext`] or touching effect APIs.

use std::path::{Path, PathBuf};

use crate::error::AuthorityError;
use crate::projection::ServiceLivenessProjection;
use crate::state::SCHEMA_VERSION;

/// Read-only probe of one authority root for internal readiness/health.
///
/// This type cannot authenticate principals, issue sessions or workspaces, seal
/// capabilities, or settle physical sends. It exists so operational probes do
/// not fabricate [`crate::AuthContext`] values.
pub struct InternalServiceAuthority {
    root: PathBuf,
}

impl std::fmt::Debug for InternalServiceAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternalServiceAuthority")
            .field("root", &"[opaque]")
            .finish()
    }
}

impl InternalServiceAuthority {
    /// Open a non-administrative probe view of `root`.
    ///
    /// Unlike [`crate::HostAuthority::open`], this does not take the exclusive
    /// admin lock and cannot administer or authenticate.
    pub fn open_probe(root: impl AsRef<Path>) -> Result<Self, AuthorityError> {
        let requested = root.as_ref();
        std::fs::create_dir_all(requested)
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        let root = dunce::canonicalize(requested)
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        Ok(Self { root })
    }

    /// Return a secret-free liveness snapshot.
    pub fn liveness_projection(&self) -> Result<ServiceLivenessProjection, AuthorityError> {
        let text = match std::fs::read_to_string(self.root.join("authority.json")) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ServiceLivenessProjection {
                    schema_version: SCHEMA_VERSION,
                    credentials_configured: false,
                    control_epoch: 0,
                    policy_revision: 0,
                    capability_generation: 0,
                });
            }
            Err(e) => return Err(AuthorityError::Durability(e.to_string())),
        };
        let state: crate::state::StoredAuthority =
            serde_json::from_str(&text).map_err(|e| AuthorityError::CorruptState(e.to_string()))?;
        if state.schema_version != SCHEMA_VERSION {
            return Err(AuthorityError::CorruptState(format!(
                "unsupported authority schema version {}",
                state.schema_version
            )));
        }
        Ok(ServiceLivenessProjection {
            schema_version: state.schema_version,
            credentials_configured: !state.credentials.is_empty(),
            control_epoch: state.control_epoch,
            policy_revision: state.policy_revision,
            capability_generation: state.capability_generation,
        })
    }
}
