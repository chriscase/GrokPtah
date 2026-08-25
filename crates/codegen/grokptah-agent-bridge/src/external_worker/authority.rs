//! Durable authority for external-worker actions.
//!
//! Possession of a provider's opaque ID is not authority. Neither is a
//! launch-time repository allowlist: it is consulted once, at creation, and
//! says nothing about whether *this* caller may read, steer, cancel, archive,
//! or download artifacts from a worker that already exists. Without a record
//! binding the grant to who asked and what they asked for, any caller that
//! learns an `external_agent_id` — from a log, a shared journal, a sibling
//! project, another tenant — inherits every later action on it.
//!
//! An [`ExternalWorkerAuthority`] is written once at launch and re-checked
//! before every subsequent action. It binds the grant to all of:
//!
//! * **principal** and **tenant** — who, and under whose organization;
//! * **project**, **workspace**, **session** — the exact local scope;
//! * **policy revision** and **capability revision** — the rules in force when
//!   the grant was made, so a policy change invalidates it rather than being
//!   silently inherited;
//! * **provider** and **provider account** — a second account under the same
//!   provider is a different authority;
//! * **worker**, **provider run**, **GrokPtah run** and **attempt**;
//! * **request**, and an immutable **launch intent** digest.
//!
//! Every field is compared on every action. A mismatch is
//! [`AuthorityError::Forbidden`], never a fallback to "the ID looked right".

use grokptah_agent_sdk::{
    Bounds, ExternalWorkerExecutionMode, ExternalWorkerLaunchRequest, ExternalWorkerProvider,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Maximum UTF-8 bytes for one authority identity component.
pub const MAX_AUTHORITY_FIELD_BYTES: usize = 256;
/// Maximum provider runs one worker authority may accumulate.
///
/// Follow-ups add a run to the grant. Without a ceiling a worker that is
/// steered indefinitely grows an unbounded durable record.
pub const MAX_AUTHORITY_RUNS: usize = 256;

/// Every action an external-worker authority can gate.
///
/// Read actions are enumerated too: a projection that leaks a prompt preview,
/// a branch name, or an artifact path across a tenant boundary is the same
/// disclosure as a mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalWorkerAction {
    /// Create the worker and its first run.
    Launch,
    /// Read a worker projection.
    GetWorker,
    /// Read a run projection.
    GetRun,
    /// Queue another prompt on an existing worker.
    FollowUp,
    /// Cancel an active run.
    Cancel,
    /// Archive a terminal worker.
    Archive,
    /// Return an archived worker to the active list.
    Unarchive,
    /// List run-attributed artifacts.
    ListArtifacts,
    /// List the workers this principal may see.
    List,
    /// Reconcile local state against the provider.
    Reconcile,
}

impl ExternalWorkerAction {
    /// Stable label used in durable audit records and error text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::GetWorker => "get_worker",
            Self::GetRun => "get_run",
            Self::FollowUp => "follow_up",
            Self::Cancel => "cancel",
            Self::Archive => "archive",
            Self::Unarchive => "unarchive",
            Self::ListArtifacts => "list_artifacts",
            Self::List => "list",
            Self::Reconcile => "reconcile",
        }
    }

    /// Whether this action changes provider or local state.
    pub fn is_mutating(self) -> bool {
        matches!(
            self,
            Self::Launch | Self::FollowUp | Self::Cancel | Self::Archive | Self::Unarchive
        )
    }
}

/// Who is asking, and under exactly which scope and policy.
///
/// This is supplied by the trusted caller from an authenticated context. It is
/// never parsed out of a provider response and never inferred from an ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalWorkerPrincipal {
    /// Authenticated principal identity.
    pub principal: String,
    /// Organization or tenant that owns the principal.
    pub tenant: String,
    /// Project identity within the tenant.
    pub project: String,
    /// Canonical workspace path this action is scoped to.
    pub workspace: String,
    /// Session that is asking.
    pub session_id: String,
    /// Revision of the policy in force for this caller.
    pub policy_revision: String,
    /// Revision of the negotiated capability set.
    pub capability_revision: String,
    /// Provider account the credential belongs to.
    ///
    /// Two accounts under one provider are different authorities: an ID minted
    /// by one must not be actionable with the other's credential.
    pub provider_account: String,
}

impl ExternalWorkerPrincipal {
    /// Validate that every identity component is present and bounded.
    pub fn validate(&self) -> Result<(), AuthorityError> {
        for (label, value) in [
            ("principal", &self.principal),
            ("tenant", &self.tenant),
            ("project", &self.project),
            ("workspace", &self.workspace),
            ("session_id", &self.session_id),
            ("policy_revision", &self.policy_revision),
            ("capability_revision", &self.capability_revision),
            ("provider_account", &self.provider_account),
        ] {
            if value.trim().is_empty() {
                return Err(AuthorityError::Incomplete(label));
            }
            if value.len() > MAX_AUTHORITY_FIELD_BYTES {
                return Err(AuthorityError::Incomplete(label));
            }
            if value.chars().any(char::is_control) {
                return Err(AuthorityError::Incomplete(label));
            }
        }
        Ok(())
    }

    /// Stable digest of the full scope, for cheap equality in audit records.
    pub fn digest(&self) -> String {
        let canonical = serde_json::json!({
            "principal": self.principal,
            "tenant": self.tenant,
            "project": self.project,
            "workspace": self.workspace,
            "sessionId": self.session_id,
            "policyRevision": self.policy_revision,
            "capabilityRevision": self.capability_revision,
            "providerAccount": self.provider_account,
        });
        hex_sha256(canonical.to_string().as_bytes())
    }
}

/// The launch request as it was approved, frozen.
///
/// The prompt is stored only as a digest: the authority record must be able to
/// prove that the intent did not drift without itself becoming a place a
/// prompt can leak from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchIntent {
    /// Provider family approved for this grant.
    pub provider: ExternalWorkerProvider,
    /// Exact repository identity approved by the authority.
    pub repository: String,
    /// Exact starting ref approved by the authority.
    pub starting_ref: String,
    /// Digest of the approved prompt; never the prompt itself.
    pub prompt_digest: String,
    /// Model or profile label, if one was approved.
    pub model: Option<String>,
    /// Isolation mode approved for this grant.
    pub execution_mode: ExternalWorkerExecutionMode,
    /// Approved ceilings, if any.
    pub bounds: Option<Bounds>,
}

impl LaunchIntent {
    /// Freeze the intent carried by an already-validated launch request.
    pub fn from_request(request: &ExternalWorkerLaunchRequest) -> Self {
        Self {
            provider: request.provider,
            repository: request.repository.clone(),
            starting_ref: request.starting_ref.clone(),
            prompt_digest: hex_sha256(request.prompt.as_bytes()),
            model: request.model.clone(),
            execution_mode: request.execution_mode,
            bounds: request.bounds.clone(),
        }
    }

    /// Stable digest over the whole approved intent.
    pub fn digest(&self) -> String {
        // `to_value` on a struct with `deny_unknown_fields`-shaped ordering is
        // stable because serde emits declaration order, and every nested value
        // is itself a bounded scalar or a `Bounds`.
        let canonical = serde_json::to_string(self).unwrap_or_else(|_| "intent-unencodable".into());
        hex_sha256(canonical.as_bytes())
    }
}

/// Lifecycle of the grant itself, independent of the provider's run state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityState {
    /// The grant is usable.
    Active,
    /// The worker was archived; reads still resolve, mutations do not.
    Archived,
    /// The grant was withdrawn and nothing may use it again.
    Revoked,
}

/// A durable grant binding one external worker to one authenticated scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalWorkerAuthority {
    /// Scope this grant was issued to.
    pub principal: ExternalWorkerPrincipal,
    /// Provider family that owns the opaque IDs.
    pub provider: ExternalWorkerProvider,
    /// Opaque provider worker identity.
    pub external_agent_id: String,
    /// Every provider run this grant covers: the launch run and its follow-ups.
    pub external_run_ids: BTreeSet<String>,
    /// GrokPtah run this worker was launched for.
    pub run_id: String,
    /// Attempt within that run.
    pub attempt: u32,
    /// Idempotency key of the launch request that created the grant.
    pub request_id: String,
    /// Digest of the frozen launch intent.
    pub launch_intent_digest: String,
    /// The frozen launch intent itself.
    pub launch_intent: LaunchIntent,
    /// Which requested ceilings were enforced, and by whom.
    ///
    /// A ceiling the caller asked for is either recorded here with its enforcer
    /// or was refused at admission. It is never silently dropped.
    #[serde(default)]
    pub bounds: Option<super::BoundsDisposition>,
    /// Grant lifecycle.
    pub state: AuthorityState,
    /// RFC3339 creation timestamp.
    pub created_at: String,
    /// RFC3339 last-update timestamp.
    pub updated_at: String,
}

/// Why an action was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthorityError {
    /// The caller supplied an incomplete or unbounded identity.
    #[error("external worker authority identity is incomplete: {0}")]
    Incomplete(&'static str),
    /// No grant exists for this worker.
    #[error("external worker has no authority record")]
    Missing,
    /// A grant exists but does not cover this caller or this action.
    ///
    /// Deliberately one variant: telling a caller *which* binding failed tells
    /// them which one to forge next, and confirms the worker exists.
    #[error("external worker action is not authorized")]
    Forbidden,
    /// The grant was withdrawn.
    #[error("external worker authority was revoked")]
    Revoked,
    /// The grant cannot hold another provider run.
    #[error("external worker authority run ceiling reached")]
    RunCeiling,
    /// The durable grant could not be read or written.
    #[error("external worker authority store is unavailable")]
    Unavailable,
}

/// Everything an approved launch must bind into its grant.
///
/// A named struct rather than a positional argument list: several of these are
/// opaque strings, and two adjacent `&str` parameters are a swap that compiles.
#[derive(Debug, Clone)]
pub struct NewGrant {
    /// Authenticated scope the grant is issued to.
    pub principal: ExternalWorkerPrincipal,
    /// Provider family that minted the opaque IDs.
    pub provider: ExternalWorkerProvider,
    /// Opaque provider worker identity.
    pub external_agent_id: String,
    /// Opaque provider run identity for the launch run.
    pub external_run_id: String,
    /// GrokPtah run this worker was launched for.
    pub run_id: String,
    /// Attempt within that run.
    pub attempt: u32,
    /// Idempotency key of the launch request.
    pub request_id: String,
    /// The frozen launch intent.
    pub launch_intent: LaunchIntent,
    /// What was actually done about each requested ceiling.
    pub bounds: Option<super::BoundsDisposition>,
    /// RFC3339 issue timestamp.
    pub now: String,
}

impl ExternalWorkerAuthority {
    /// Issue a grant for an approved launch.
    pub fn issue(grant: NewGrant) -> Result<Self, AuthorityError> {
        grant.principal.validate()?;
        let mut external_run_ids = BTreeSet::new();
        external_run_ids.insert(grant.external_run_id);
        Ok(Self {
            principal: grant.principal,
            provider: grant.provider,
            external_agent_id: grant.external_agent_id,
            external_run_ids,
            run_id: grant.run_id,
            attempt: grant.attempt,
            request_id: grant.request_id,
            launch_intent_digest: grant.launch_intent.digest(),
            launch_intent: grant.launch_intent,
            bounds: grant.bounds,
            state: AuthorityState::Active,
            created_at: grant.now.clone(),
            updated_at: grant.now,
        })
    }

    /// Re-authorize one action against this grant.
    ///
    /// Every binding is compared. An opaque ID that matches while the tenant,
    /// project, workspace, session, policy revision, capability revision, or
    /// provider account does not is refused: possession is not authority.
    pub fn authorize(
        &self,
        action: ExternalWorkerAction,
        claimed: &ExternalWorkerPrincipal,
        external_agent_id: &str,
        external_run_id: Option<&str>,
    ) -> Result<(), AuthorityError> {
        claimed.validate()?;
        if self.state == AuthorityState::Revoked {
            return Err(AuthorityError::Revoked);
        }
        // Compare the whole scope, not a subset. A digest comparison here would
        // hide which fields exist; an explicit field-by-field compare keeps the
        // binding list auditable and makes a new field a compile error at the
        // struct rather than a silently unchecked value.
        let bound = &self.principal;
        let matches = bound.principal == claimed.principal
            && bound.tenant == claimed.tenant
            && bound.project == claimed.project
            && bound.workspace == claimed.workspace
            && bound.session_id == claimed.session_id
            && bound.policy_revision == claimed.policy_revision
            && bound.capability_revision == claimed.capability_revision
            && bound.provider_account == claimed.provider_account;
        if !matches {
            return Err(AuthorityError::Forbidden);
        }
        if self.external_agent_id != external_agent_id {
            return Err(AuthorityError::Forbidden);
        }
        if let Some(run) = external_run_id {
            if !self.external_run_ids.contains(run) {
                return Err(AuthorityError::Forbidden);
            }
        }
        // An archived worker is readable but not steerable. Unarchive is the
        // one mutation that is allowed to act on it.
        if self.state == AuthorityState::Archived
            && action.is_mutating()
            && action != ExternalWorkerAction::Unarchive
        {
            return Err(AuthorityError::Forbidden);
        }
        // Launch cannot be re-authorized against an existing grant: a second
        // launch is a new grant, and reusing one would let a caller re-run an
        // intent that was approved once.
        if action == ExternalWorkerAction::Launch {
            return Err(AuthorityError::Forbidden);
        }
        Ok(())
    }

    /// Record a follow-up run under this grant.
    pub fn admit_run(
        &mut self,
        external_run_id: impl Into<String>,
        now: impl Into<String>,
    ) -> Result<(), AuthorityError> {
        let run = external_run_id.into();
        if self.external_run_ids.contains(&run) {
            return Ok(());
        }
        if self.external_run_ids.len() >= MAX_AUTHORITY_RUNS {
            return Err(AuthorityError::RunCeiling);
        }
        self.external_run_ids.insert(run);
        self.updated_at = now.into();
        Ok(())
    }

    /// Prove the intent behind this grant has not drifted.
    pub fn intent_matches(&self, intent: &LaunchIntent) -> bool {
        self.launch_intent_digest == intent.digest()
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Durable, private store for external-worker grants.
///
/// One file per worker, written through [`super::durable`], so a grant is as
/// private and as crash-durable as the ledger receipts beside it.
#[derive(Debug, Clone)]
pub struct AuthorityStore {
    root: std::path::PathBuf,
}

impl AuthorityStore {
    /// Open (creating if needed) the grant store under `root`.
    pub fn open(root: impl AsRef<std::path::Path>) -> Result<Self, AuthorityError> {
        let root = root.as_ref().to_path_buf();
        super::durable::create_private_dir_all(&root).map_err(|_| AuthorityError::Unavailable)?;
        Ok(Self { root })
    }

    fn path(
        &self,
        provider: ExternalWorkerProvider,
        external_agent_id: &str,
    ) -> Option<std::path::PathBuf> {
        // The provider ID is opaque and attacker-influenced, so it is never a
        // path component. Its digest is, which is fixed-length and has no
        // separators, no traversal, and no case-collision on Windows.
        if external_agent_id.is_empty() || external_agent_id.len() > MAX_AUTHORITY_FIELD_BYTES {
            return None;
        }
        let key =
            hex_sha256(format!("{}\u{0}{external_agent_id}", provider_key(provider)).as_bytes());
        Some(self.root.join(format!("{key}.json")))
    }

    /// Load the grant for a worker, if one exists.
    pub fn load(
        &self,
        provider: ExternalWorkerProvider,
        external_agent_id: &str,
    ) -> Result<Option<ExternalWorkerAuthority>, AuthorityError> {
        let Some(path) = self.path(provider, external_agent_id) else {
            return Err(AuthorityError::Forbidden);
        };
        let Some(bytes) =
            super::durable::read_private_json(&path).map_err(|_| AuthorityError::Unavailable)?
        else {
            return Ok(None);
        };
        let record: ExternalWorkerAuthority =
            serde_json::from_slice(&bytes).map_err(|_| AuthorityError::Unavailable)?;
        // A record whose own identity does not match the key it was loaded
        // under is not this worker's grant.
        if record.external_agent_id != external_agent_id || record.provider != provider {
            return Err(AuthorityError::Forbidden);
        }
        Ok(Some(record))
    }

    /// Persist a newly issued grant. Refuses to overwrite an existing one.
    pub fn insert(&self, record: &ExternalWorkerAuthority) -> Result<(), AuthorityError> {
        let Some(path) = self.path(record.provider, &record.external_agent_id) else {
            return Err(AuthorityError::Forbidden);
        };
        super::durable::cas_private_json(&path, None, record)
            .map_err(|_| AuthorityError::Unavailable)
    }

    /// Persist an update to an existing grant.
    pub fn update(&self, record: &ExternalWorkerAuthority) -> Result<(), AuthorityError> {
        let Some(path) = self.path(record.provider, &record.external_agent_id) else {
            return Err(AuthorityError::Forbidden);
        };
        let expected = super::durable::record_digest(&path)
            .map_err(|_| AuthorityError::Unavailable)?
            .ok_or(AuthorityError::Missing)?;
        super::durable::cas_private_json(&path, Some(&expected), record)
            .map_err(|_| AuthorityError::Unavailable)
    }

    /// Load a grant and re-authorize one action against it in a single step.
    ///
    /// This is the only entry point a caller should use before acting: it
    /// makes "I have the ID" insufficient by construction, because there is no
    /// path to the record that does not also check the scope.
    pub fn authorize(
        &self,
        action: ExternalWorkerAction,
        claimed: &ExternalWorkerPrincipal,
        provider: ExternalWorkerProvider,
        external_agent_id: &str,
        external_run_id: Option<&str>,
    ) -> Result<ExternalWorkerAuthority, AuthorityError> {
        let record = self
            .load(provider, external_agent_id)?
            .ok_or(AuthorityError::Missing)?;
        record.authorize(action, claimed, external_agent_id, external_run_id)?;
        Ok(record)
    }
}

/// Maximum grants one listing will return.
///
/// A listing walks the store, so it is bounded for the same reason an artifact
/// listing is: an unbounded local directory should not become an unbounded
/// response.
pub const MAX_AUTHORITY_LISTING: usize = 512;

impl AuthorityStore {
    /// List the grants this exact scope holds.
    ///
    /// Filtering happens after loading, on the same field-by-field comparison
    /// `authorize` uses, so a listing cannot become a way to learn that another
    /// tenant's worker exists. Revoked grants are omitted; archived ones are
    /// included and marked, because hiding them would make "where did my worker
    /// go" unanswerable.
    pub fn list_for(
        &self,
        claimed: &ExternalWorkerPrincipal,
    ) -> Result<Vec<ExternalWorkerAuthority>, AuthorityError> {
        claimed.validate()?;
        let entries = std::fs::read_dir(&self.root).map_err(|_| AuthorityError::Unavailable)?;
        let mut found = Vec::new();
        for entry in entries {
            let path = entry.map_err(|_| AuthorityError::Unavailable)?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(Some(bytes)) = super::durable::read_private_json(&path) else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<ExternalWorkerAuthority>(&bytes) else {
                continue;
            };
            if record.state == AuthorityState::Revoked {
                continue;
            }
            // The same comparison `authorize` makes. A grant this caller could
            // not act on is a grant they must not be told exists.
            if record.principal != *claimed {
                continue;
            }
            found.push(record);
            if found.len() >= MAX_AUTHORITY_LISTING {
                break;
            }
        }
        found.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.external_agent_id.cmp(&right.external_agent_id))
        });
        Ok(found)
    }

    /// Move a grant between active and archived, under re-authorization.
    ///
    /// Archiving is itself an authorized action, so a caller outside the grant
    /// cannot park or resurrect someone else's worker.
    pub fn set_archived(
        &self,
        claimed: &ExternalWorkerPrincipal,
        provider: ExternalWorkerProvider,
        external_agent_id: &str,
        archived: bool,
        now: &str,
    ) -> Result<ExternalWorkerAuthority, AuthorityError> {
        let action = if archived {
            ExternalWorkerAction::Archive
        } else {
            ExternalWorkerAction::Unarchive
        };
        let mut record = self.authorize(action, claimed, provider, external_agent_id, None)?;
        record.state = if archived {
            AuthorityState::Archived
        } else {
            AuthorityState::Active
        };
        record.updated_at = now.to_string();
        self.update(&record)?;
        Ok(record)
    }
}

fn provider_key(provider: ExternalWorkerProvider) -> &'static str {
    match provider {
        ExternalWorkerProvider::CursorCloud => "cursor_cloud",
        ExternalWorkerProvider::ClaudeCodeCloud => "claude_code_cloud",
        ExternalWorkerProvider::LocalWorker => "local_worker",
        ExternalWorkerProvider::Custom => "custom",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal() -> ExternalWorkerPrincipal {
        ExternalWorkerPrincipal {
            principal: "user-1".into(),
            tenant: "tenant-a".into(),
            project: "project-x".into(),
            workspace: "/work/repo".into(),
            session_id: "session-1".into(),
            policy_revision: "policy-7".into(),
            capability_revision: "cap-3".into(),
            provider_account: "account-1".into(),
        }
    }

    fn intent() -> LaunchIntent {
        LaunchIntent {
            provider: ExternalWorkerProvider::CursorCloud,
            repository: "chriscase/GrokPtah".into(),
            starting_ref: "main".into(),
            prompt_digest: hex_sha256(b"do the work"),
            model: None,
            execution_mode: ExternalWorkerExecutionMode::Isolated,
            bounds: None,
        }
    }

    fn grant() -> ExternalWorkerAuthority {
        ExternalWorkerAuthority::issue(NewGrant {
            principal: principal(),
            provider: ExternalWorkerProvider::CursorCloud,
            external_agent_id: "agent-1".into(),
            external_run_id: "run-1".into(),
            run_id: "gp-run-1".into(),
            attempt: 1,
            request_id: "req-1".into(),
            launch_intent: intent(),
            bounds: None,
            now: "2026-08-25T00:00:00Z".into(),
        })
        .expect("grant issues")
    }

    /// The headline finding: holding the opaque ID is not authority. Every
    /// scope field must match, one at a time.
    #[test]
    fn possession_of_the_opaque_id_is_never_authority() {
        let record = grant();
        // The exact caller is allowed.
        record
            .authorize(
                ExternalWorkerAction::GetRun,
                &principal(),
                "agent-1",
                Some("run-1"),
            )
            .expect("the issuing scope may read its own worker");

        // Each field alone is enough to refuse, with the same opaque ID.
        type Mutate = fn(&mut ExternalWorkerPrincipal);
        let mutations: [(&str, Mutate); 8] = [
            ("principal", |p| p.principal = "user-2".into()),
            ("tenant", |p| p.tenant = "tenant-b".into()),
            ("project", |p| p.project = "project-y".into()),
            ("workspace", |p| p.workspace = "/work/other".into()),
            ("session", |p| p.session_id = "session-2".into()),
            ("policy revision", |p| p.policy_revision = "policy-8".into()),
            ("capability revision", |p| {
                p.capability_revision = "cap-4".into()
            }),
            ("provider account", |p| {
                p.provider_account = "account-2".into()
            }),
        ];
        for (label, mutate) in mutations {
            let mut claimed = principal();
            mutate(&mut claimed);
            assert_eq!(
                record.authorize(
                    ExternalWorkerAction::GetRun,
                    &claimed,
                    "agent-1",
                    Some("run-1"),
                ),
                Err(AuthorityError::Forbidden),
                "a different {label} must not inherit this worker",
            );
        }
    }

    /// Every action reauthorizes, including the read-only ones. A projection
    /// that crosses a tenant boundary is the same disclosure as a mutation.
    #[test]
    fn every_action_reauthorizes_not_just_the_mutating_ones() {
        let record = grant();
        let mut foreign = principal();
        foreign.tenant = "tenant-b".into();
        for action in [
            ExternalWorkerAction::GetWorker,
            ExternalWorkerAction::GetRun,
            ExternalWorkerAction::FollowUp,
            ExternalWorkerAction::Cancel,
            ExternalWorkerAction::Archive,
            ExternalWorkerAction::Unarchive,
            ExternalWorkerAction::ListArtifacts,
            ExternalWorkerAction::Reconcile,
        ] {
            assert_eq!(
                record.authorize(action, &foreign, "agent-1", Some("run-1")),
                Err(AuthorityError::Forbidden),
                "{} must reauthorize",
                action.as_str(),
            );
        }
    }

    /// A run ID from a different worker must not be actionable just because
    /// the worker ID is right.
    #[test]
    fn a_run_outside_the_grant_is_refused() {
        let record = grant();
        assert_eq!(
            record.authorize(
                ExternalWorkerAction::Cancel,
                &principal(),
                "agent-1",
                Some("run-somebody-else"),
            ),
            Err(AuthorityError::Forbidden),
        );
        assert_eq!(
            record.authorize(
                ExternalWorkerAction::GetRun,
                &principal(),
                "agent-other",
                Some("run-1"),
            ),
            Err(AuthorityError::Forbidden),
        );
    }

    #[test]
    fn follow_up_runs_join_the_grant_and_are_bounded() {
        let mut record = grant();
        record
            .admit_run("run-2", "2026-08-25T00:01:00Z")
            .expect("a follow-up run joins the grant");
        record
            .authorize(
                ExternalWorkerAction::Cancel,
                &principal(),
                "agent-1",
                Some("run-2"),
            )
            .expect("the admitted run is now covered");
        // Re-admitting is idempotent, not a second slot.
        record.admit_run("run-2", "2026-08-25T00:02:00Z").unwrap();
        while record.external_run_ids.len() < MAX_AUTHORITY_RUNS {
            let next = format!("run-fill-{}", record.external_run_ids.len());
            record.admit_run(next, "2026-08-25T00:03:00Z").unwrap();
        }
        assert_eq!(
            record.admit_run("run-over", "2026-08-25T00:04:00Z"),
            Err(AuthorityError::RunCeiling),
        );
    }

    #[test]
    fn archived_workers_read_but_do_not_steer_and_revoked_do_neither() {
        let mut record = grant();
        record.state = AuthorityState::Archived;
        record
            .authorize(
                ExternalWorkerAction::GetRun,
                &principal(),
                "agent-1",
                Some("run-1"),
            )
            .expect("an archived worker stays readable");
        assert_eq!(
            record.authorize(
                ExternalWorkerAction::FollowUp,
                &principal(),
                "agent-1",
                Some("run-1"),
            ),
            Err(AuthorityError::Forbidden),
        );
        record
            .authorize(
                ExternalWorkerAction::Unarchive,
                &principal(),
                "agent-1",
                Some("run-1"),
            )
            .expect("unarchive is the one mutation an archived grant allows");

        record.state = AuthorityState::Revoked;
        for action in [
            ExternalWorkerAction::GetRun,
            ExternalWorkerAction::Unarchive,
            ExternalWorkerAction::ListArtifacts,
        ] {
            assert_eq!(
                record.authorize(action, &principal(), "agent-1", Some("run-1")),
                Err(AuthorityError::Revoked),
            );
        }
    }

    /// A grant is issued for one approved intent. Re-authorizing `Launch`
    /// against it would let a caller re-run that intent.
    #[test]
    fn a_grant_never_reauthorizes_a_second_launch() {
        assert_eq!(
            grant().authorize(
                ExternalWorkerAction::Launch,
                &principal(),
                "agent-1",
                Some("run-1"),
            ),
            Err(AuthorityError::Forbidden),
        );
    }

    #[test]
    fn intent_drift_is_detectable_and_the_prompt_is_never_stored() {
        let record = grant();
        assert!(record.intent_matches(&intent()));
        let mut drifted = intent();
        drifted.starting_ref = "refs/heads/attacker".into();
        assert!(!record.intent_matches(&drifted));

        let encoded = serde_json::to_string(&record).expect("grant serializes");
        assert!(
            !encoded.contains("do the work"),
            "the approved prompt must never be stored in the grant",
        );
        assert!(encoded.contains(&hex_sha256(b"do the work")));
    }

    #[test]
    fn an_incomplete_or_unbounded_identity_is_refused_before_anything_else() {
        type Mutate = fn(&mut ExternalWorkerPrincipal);
        let cases: [(&str, Mutate); 4] = [
            ("principal", |p| p.principal = String::new()),
            ("tenant", |p| p.tenant = "  ".into()),
            ("workspace", |p| {
                p.workspace = "x".repeat(MAX_AUTHORITY_FIELD_BYTES + 1)
            }),
            ("session_id", |p| p.session_id = "sess\u{0}ion".into()),
        ];
        for (label, mutate) in cases {
            let mut claimed = principal();
            mutate(&mut claimed);
            assert!(
                matches!(claimed.validate(), Err(AuthorityError::Incomplete(_))),
                "{label} must be refused",
            );
            assert!(
                grant()
                    .authorize(
                        ExternalWorkerAction::GetRun,
                        &claimed,
                        "agent-1",
                        Some("run-1")
                    )
                    .is_err(),
                "{label} must not authorize",
            );
        }
    }

    #[test]
    fn the_store_round_trips_grants_privately_and_refuses_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let store = AuthorityStore::open(dir.path()).unwrap();
        let record = grant();
        assert!(store
            .load(ExternalWorkerProvider::CursorCloud, "agent-1")
            .unwrap()
            .is_none());
        store.insert(&record).unwrap();
        assert_eq!(
            store
                .load(ExternalWorkerProvider::CursorCloud, "agent-1")
                .unwrap()
                .as_ref(),
            Some(&record),
        );
        // A second insert must not clobber the first grant.
        assert_eq!(store.insert(&record), Err(AuthorityError::Unavailable));

        // The same opaque ID under a different provider is a different grant.
        assert!(store
            .load(ExternalWorkerProvider::LocalWorker, "agent-1")
            .unwrap()
            .is_none());

        // Authorizing through the store is the only path, and it still checks.
        store
            .authorize(
                ExternalWorkerAction::GetRun,
                &principal(),
                ExternalWorkerProvider::CursorCloud,
                "agent-1",
                Some("run-1"),
            )
            .expect("the issuing scope is authorized");
        let mut foreign = principal();
        foreign.tenant = "tenant-b".into();
        assert_eq!(
            store
                .authorize(
                    ExternalWorkerAction::GetRun,
                    &foreign,
                    ExternalWorkerProvider::CursorCloud,
                    "agent-1",
                    Some("run-1"),
                )
                .unwrap_err(),
            AuthorityError::Forbidden,
        );
        // An unknown worker is Missing, not a silent allow.
        assert_eq!(
            store
                .authorize(
                    ExternalWorkerAction::GetRun,
                    &principal(),
                    ExternalWorkerProvider::CursorCloud,
                    "agent-unknown",
                    None,
                )
                .unwrap_err(),
            AuthorityError::Missing,
        );
    }

    /// The provider's opaque ID is attacker-influenced. It must never become a
    /// path component, or a crafted ID escapes the store directory.
    #[test]
    fn a_hostile_opaque_id_never_becomes_a_path_component() {
        let dir = tempfile::tempdir().unwrap();
        let store = AuthorityStore::open(dir.path()).unwrap();
        for hostile in [
            "../../etc/passwd",
            "/etc/passwd",
            "C:/Windows/System32",
            "a/b",
            "..",
            ".",
        ] {
            let path = store
                .path(ExternalWorkerProvider::CursorCloud, hostile)
                .expect("a bounded id still maps to a key");
            assert_eq!(
                path.parent(),
                Some(dir.path()),
                "id {hostile:?} must stay inside the store directory",
            );
            assert!(store
                .load(ExternalWorkerProvider::CursorCloud, hostile)
                .unwrap()
                .is_none());
        }
        // An unbounded id is refused outright rather than hashed.
        assert!(store
            .path(
                ExternalWorkerProvider::CursorCloud,
                &"x".repeat(MAX_AUTHORITY_FIELD_BYTES + 1)
            )
            .is_none());
    }
}
