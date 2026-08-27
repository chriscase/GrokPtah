//! Durable authority binding for orchestration operations and scoped reads.
//!
//! [`authz`](super::authz) answers *who is calling* (bearer credential →
//! [`AuthContext`]). This module answers the second half of the question:
//! *whose durable record is this, and may the caller see it at all*.
//!
//! Every durable operation record (idempotency receipt, provider attempt)
//! carries a [`PrincipalScope`] describing the credential, owner, session, and
//! workspace it was created under. Reads compare the caller's authority
//! against that binding through [`PrincipalScope::authorize_read`] and fail
//! closed with a single, deliberately uninformative denial so an unauthorized
//! caller cannot distinguish "not yours" from "does not exist".
//!
//! Two boundaries exist, and they are not the same:
//!
//! * **Owner** (`AuthContext::owner_id`) is the tenancy boundary. Runs are a
//!   shared session artifact: every device credential of one owner is expected
//!   to observe them, so runs authorize on owner + session + workspace.
//! * **Credential** (`AuthContext::token_id`) is the operation boundary. A
//!   receipt and a provider attempt are artifacts of one specific caller's
//!   mutation, so they additionally require the *same* credential. This is what
//!   keeps two device credentials of one owner from replaying or reading each
//!   other's operation payloads.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::authz::{constant_time_eq, AuthContext};
use super::types::{OrchError, OrchErrorCode};

/// The durable authority binding stamped onto an operation record.
///
/// This is identity, never secret material: `token_id` is the credential's
/// stable name (see [`super::authz::AuthCredential`]), not its token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrincipalScope {
    /// Tenancy identity. Records created by one owner are never readable by
    /// another owner, regardless of credential or session.
    pub owner_id: String,
    /// Credential identity that performed the operation.
    pub token_id: String,
    /// Session the operation was authorized under, when the operation is
    /// session-scoped. Host-internal operations carry `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<uuid::Uuid>,
    /// Canonical workspace the operation was authorized under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// How strictly a read must match the record's recorded principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeStrictness {
    /// Owner must match; any credential of that owner is accepted. Used for
    /// runs, which are a shared per-session artifact.
    Owner,
    /// Owner *and* credential must match. Used for operation records
    /// (receipts, provider attempts) that belong to one specific caller.
    Credential,
}

impl PrincipalScope {
    /// Bind a scope from an authenticated caller and its authorized scope.
    pub fn bind(
        auth: &AuthContext,
        session_id: Option<uuid::Uuid>,
        workspace: Option<&Path>,
    ) -> Self {
        Self {
            owner_id: auth.owner_id.clone(),
            token_id: auth.token_id.clone(),
            session_id,
            workspace: workspace.map(|path| path.display().to_string()),
        }
    }

    /// Authorize a read of this record by `auth` under `strictness`.
    ///
    /// Every failure returns the same [`denied`] error: an unauthorized caller
    /// learns only that nothing readable exists at the requested id.
    pub fn authorize_read(
        &self,
        auth: &AuthContext,
        strictness: ScopeStrictness,
    ) -> Result<(), OrchError> {
        if !identity_eq(&self.owner_id, &auth.owner_id) {
            return Err(denied());
        }
        if strictness == ScopeStrictness::Credential && !identity_eq(&self.token_id, &auth.token_id)
        {
            return Err(denied());
        }
        Ok(())
    }

    /// Authorize a read that must also match an exact session and workspace.
    ///
    /// `claimed` is the already-canonicalized workspace the caller proved it
    /// may address; passing an unvalidated caller string here would defeat the
    /// check, so callers resolve the workspace through the existing
    /// allowlist/session match first.
    pub fn authorize_scoped_read(
        &self,
        auth: &AuthContext,
        strictness: ScopeStrictness,
        session_id: uuid::Uuid,
        claimed: &Path,
    ) -> Result<(), OrchError> {
        self.authorize_read(auth, strictness)?;
        if self.session_id.is_some_and(|bound| bound != session_id) {
            return Err(denied());
        }
        let claimed = claimed.display().to_string();
        if self
            .workspace
            .as_deref()
            .is_some_and(|bound| !super::workspaces_match(bound, &claimed))
        {
            return Err(denied());
        }
        Ok(())
    }
}

/// Authorize a record whose principal binding may predate this seam.
///
/// A record written before principal binding existed carries no scope. Such a
/// record was necessarily created by the service's single configured owner, so
/// it is attributed to the *caller's* owner and authorized on that basis
/// rather than being made unreadable. This is the conservative reading while
/// one service instance serves one owner; a service that ever maps credentials
/// to more than one owner must stop trusting the legacy fallback, and the
/// [`ScopeStrictness::Credential`] surfaces below already refuse unbound
/// records outright so no operation payload rides on that fallback.
pub fn authorize_optional_scope(
    scope: Option<&PrincipalScope>,
    auth: &AuthContext,
    strictness: ScopeStrictness,
) -> Result<(), OrchError> {
    match scope {
        Some(scope) => scope.authorize_read(auth, strictness),
        // An operation record without a binding cannot prove it belongs to the
        // caller, so credential-strict surfaces deny it.
        None if strictness == ScopeStrictness::Credential => Err(denied()),
        None => Ok(()),
    }
}

/// Identity namespace for operations the host performs on its own behalf.
///
/// A desktop-initiated resume has no bearer credential, but its durable
/// receipt still needs an authority binding or it would be unreadable *and*
/// unreplayable. Host operations therefore live in their own owner namespace,
/// deliberately distinct from every bearer credential: an MCP caller listing
/// its receipts never sees desktop operations, and vice versa.
pub const HOST_PRINCIPAL_OWNER_ID: &str = "host.local";
pub const HOST_PRINCIPAL_TOKEN_ID: &str = "host.local";

/// The authority binding for a host-internal operation.
pub fn host_principal(session_id: Option<uuid::Uuid>) -> PrincipalScope {
    PrincipalScope {
        owner_id: HOST_PRINCIPAL_OWNER_ID.into(),
        token_id: HOST_PRINCIPAL_TOKEN_ID.into(),
        session_id,
        workspace: None,
    }
}

/// Reconstruct the authority binding a run was created under.
///
/// `client_id` carries the creating credential's id on the wire, with one
/// historical exception: the compatibility credential `primary` is emitted as
/// `mcp`. Inverting that here is what lets the credential that started a run
/// list the provider attempts that run produced.
///
/// A run with no binding at all was created by the desktop rather than through
/// a bearer credential, so its attempts belong to the host namespace and are
/// invisible to every MCP credential — which is the correct answer, not a gap.
pub fn run_principal(
    owner_id: Option<&str>,
    client_id: Option<&str>,
    session_id: uuid::Uuid,
    workspace: &str,
) -> PrincipalScope {
    PrincipalScope {
        owner_id: owner_id.unwrap_or(HOST_PRINCIPAL_OWNER_ID).to_string(),
        token_id: match client_id {
            Some("mcp") => "primary".to_string(),
            Some(other) => other.to_string(),
            None => HOST_PRINCIPAL_TOKEN_ID.to_string(),
        },
        session_id: Some(session_id),
        workspace: Some(workspace.to_string()),
    }
}

/// The single denial returned by every authority failure on a read.
///
/// Callers must not vary this message, add the requested id to it, or pick a
/// different code by failure reason: "you may not see this" and "this does not
/// exist" are deliberately the same answer.
pub fn denied() -> OrchError {
    OrchError::new(
        OrchErrorCode::InvalidRequest,
        "no such record in the requested scope",
    )
}

/// Compare two identity strings without leaking their length relationship
/// through early exit on the common-prefix case.
fn identity_eq(left: &str, right: &str) -> bool {
    constant_time_eq(left.as_bytes(), right.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn auth(token: &str, owner: &str) -> AuthContext {
        AuthContext {
            token_id: token.into(),
            owner_id: owner.into(),
        }
    }

    fn scope(token: &str, owner: &str) -> PrincipalScope {
        PrincipalScope {
            owner_id: owner.into(),
            token_id: token.into(),
            session_id: None,
            workspace: None,
        }
    }

    #[test]
    fn owner_strictness_admits_sibling_credentials_of_one_owner() {
        let bound = scope("laptop", "account-1");
        assert!(bound
            .authorize_read(&auth("desktop", "account-1"), ScopeStrictness::Owner)
            .is_ok());
        assert!(bound
            .authorize_read(&auth("desktop", "account-2"), ScopeStrictness::Owner)
            .is_err());
    }

    #[test]
    fn credential_strictness_denies_sibling_credentials() {
        let bound = scope("laptop", "account-1");
        assert!(bound
            .authorize_read(&auth("laptop", "account-1"), ScopeStrictness::Credential)
            .is_ok());
        assert!(bound
            .authorize_read(&auth("desktop", "account-1"), ScopeStrictness::Credential)
            .is_err());
    }

    #[test]
    fn every_denial_is_byte_identical() {
        let bound = scope("laptop", "account-1");
        let wrong_owner = bound
            .authorize_read(&auth("laptop", "account-2"), ScopeStrictness::Credential)
            .unwrap_err();
        let wrong_credential = bound
            .authorize_read(&auth("desktop", "account-1"), ScopeStrictness::Credential)
            .unwrap_err();
        let unbound = authorize_optional_scope(
            None,
            &auth("laptop", "account-1"),
            ScopeStrictness::Credential,
        )
        .unwrap_err();
        for error in [&wrong_owner, &wrong_credential, &unbound] {
            assert_eq!(error.code, denied().code);
            assert_eq!(error.message, denied().message);
            assert!(error.data.is_none(), "denial must not carry extra data");
        }
    }

    #[test]
    fn scoped_read_requires_exact_session_and_workspace() {
        let session = uuid::Uuid::new_v4();
        let bound = PrincipalScope {
            owner_id: "account-1".into(),
            token_id: "laptop".into(),
            session_id: Some(session),
            workspace: Some("/tmp/project".into()),
        };
        let caller = auth("laptop", "account-1");
        let workspace = PathBuf::from("/tmp/project");
        assert!(bound
            .authorize_scoped_read(&caller, ScopeStrictness::Credential, session, &workspace)
            .is_ok());
        assert!(bound
            .authorize_scoped_read(
                &caller,
                ScopeStrictness::Credential,
                uuid::Uuid::new_v4(),
                &workspace
            )
            .is_err());
        assert!(bound
            .authorize_scoped_read(
                &caller,
                ScopeStrictness::Credential,
                session,
                Path::new("/tmp/other")
            )
            .is_err());
    }

    #[test]
    fn legacy_unbound_records_stay_readable_only_under_owner_strictness() {
        let caller = auth("laptop", "account-1");
        assert!(authorize_optional_scope(None, &caller, ScopeStrictness::Owner).is_ok());
        assert!(authorize_optional_scope(None, &caller, ScopeStrictness::Credential).is_err());
    }
}
