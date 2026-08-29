//! The canonical host-issued principal and authentication-generation fence (#477).
//!
//! Everything that answers "who is calling, and is that answer still good?"
//! lives in this module and nowhere else. There is exactly one fence: no other
//! module may define an `AuthContext`, a `VerifiedPrincipal`, a
//! `PrincipalScope`, or a parallel epoch/generation counter.
//!
//! # Why the types are opaque
//!
//! Every identity value below has private fields and no public constructor.
//! A caller outside this module cannot write `AuthContext { .. }`, cannot
//! mutate `owner_id` on a context it legitimately holds, and cannot mint a
//! `VerifiedPrincipal` for a session it was never authorized for. The only way
//! to obtain one is to present a bearer credential to the
//! `OrchestrationService`, which stamps its own live generation onto the
//! result. Authority is *issued*, never *asserted*.
//!
//! # The three layers
//!
//! * [`AuthGeneration`] — the host's live authority stamp: an authority
//!   lineage id plus a monotonic authentication epoch and policy revision.
//!   Advancing it is what makes already-issued identities stop working.
//! * [`AuthContext`] — one authenticated caller: credential identity *and
//!   incarnation*, owner/tenant, canonical public principal alias, and the
//!   generation it was issued under.
//! * [`VerifiedPrincipal`] — one authenticated caller that has additionally
//!   been authorized for an exact session and canonical workspace. Effect and
//!   read boundaries that touch stored resources take this, not a bare context,
//!   so "authenticated" can never be mistaken for "authorized here".
//!
//! # Provenance is not authority
//!
//! [`PrincipalProvenance`] records the epoch and policy revision a durable
//! record was stamped under. It is written, audited and surfaced, but it never
//! grants anything: a higher epoch does not make a record more privileged, and
//! a lower one does not make it someone else's. (Concept taken from draft #474;
//! its duplicate epoch type is deliberately not taken.)
//!
//! # Restart
//!
//! The generation is durable. On restart the host re-adopts its authority
//! lineage and advances the epoch once, so no identity minted before the
//! restart is current, and re-registering a credential id that was previously
//! removed yields a fresh *incarnation* that cannot inherit the removed one's
//! work. If the durable record cannot be read or written, the host
//! re-establishes a brand-new authority lineage and says so, which invalidates
//! everything rather than guessing.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::types::{OrchError, OrchErrorCode};

/// Wire principal emitted for the compatibility `primary` credential.
///
/// Durable records written before named device credentials existed carry this
/// value, so the normalization has to keep producing it.
pub const COMPAT_PRIMARY_PRINCIPAL: &str = "mcp";

/// The in-process managed executor's service principal.
pub const NATIVE_EXECUTOR_PRINCIPAL: &str = "native-executor";

/// Maximum lifetime a delegation may be minted for.
pub const MAX_DELEGATION_TTL_SECONDS: i64 = 900;

// ── generation ──────────────────────────────────────────────────────────────

/// The host's live authority stamp.
///
/// `authority` identifies one authority lineage — one host, across restarts.
/// `epoch` is that lineage's monotonic authentication generation, advanced by
/// every credential, owner, policy or allowlist mutation. `policy_revision` is
/// the subset of those advances that changed *policy* (allowlist or authority
/// policy) rather than only credentials, so a record's provenance can say which
/// policy it was admitted under without a second counter's worth of drift.
///
/// An identity is current only while all three halves still match the issuing
/// host, so an identity minted before any rotation stops being usable the
/// moment that rotation lands.
///
/// The stamp carries no bearer material. It is an opaque triple, and knowing it
/// does not let a caller construct one: every constructor below is
/// crate-internal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthGeneration {
    authority: Uuid,
    epoch: u64,
    policy_revision: u64,
}

impl AuthGeneration {
    /// First generation of a freshly minted authority lineage.
    ///
    /// Each call produces a lineage no other host can match, so an identity
    /// issued by one host is never current at another.
    pub(super) fn new_authority() -> Self {
        Self {
            authority: Uuid::new_v4(),
            epoch: 0,
            policy_revision: 0,
        }
    }

    /// Re-adopt a persisted lineage after restart.
    pub(super) fn adopt(authority: Uuid, epoch: u64, policy_revision: u64) -> Self {
        Self {
            authority,
            epoch,
            policy_revision,
        }
    }

    /// Next authentication epoch of the same lineage.
    ///
    /// Overflow fails closed with an error instead of saturating or wrapping; a
    /// wrapped counter would silently make already-issued stale identities
    /// current again.
    pub(super) fn next_epoch(self) -> Result<Self, OrchError> {
        let epoch = self.epoch.checked_add(1).ok_or_else(Self::exhausted)?;
        Ok(Self { epoch, ..self })
    }

    /// Next epoch *and* policy revision of the same lineage.
    ///
    /// Both are checked before either is applied, so an exhausted counter
    /// leaves the caller's state exactly as it was.
    pub(super) fn next_policy(self) -> Result<Self, OrchError> {
        let epoch = self.epoch.checked_add(1).ok_or_else(Self::exhausted)?;
        let policy_revision = self
            .policy_revision
            .checked_add(1)
            .ok_or_else(Self::exhausted)?;
        Ok(Self {
            epoch,
            policy_revision,
            ..self
        })
    }

    fn exhausted() -> OrchError {
        OrchError::new(
            OrchErrorCode::Internal,
            "authentication generation exhausted; refusing to rotate credentials or policy",
        )
    }

    /// Monotonic authentication epoch, for diagnostics and provenance. The
    /// lineage id is deliberately not exposed.
    pub fn epoch(self) -> u64 {
        self.epoch
    }

    /// Monotonic policy revision, for diagnostics and provenance.
    pub fn policy_revision(self) -> u64 {
        self.policy_revision
    }

    /// The provenance stamp to record on durable work admitted under this
    /// generation.
    pub fn provenance(self) -> PrincipalProvenance {
        PrincipalProvenance {
            epoch: self.epoch,
            policy_revision: self.policy_revision,
        }
    }

    /// Durable form. Crate-internal: this is the only way the lineage id leaves
    /// the fence, and it goes straight to the host's own private state file.
    pub(super) fn to_durable(self) -> DurableAuthority {
        DurableAuthority {
            authority: self.authority,
            epoch: self.epoch,
            policy_revision: self.policy_revision,
        }
    }

    /// Test-only: the same lineage pinned to the last representable epoch, so
    /// exhaustion can be exercised without 2^64 rotations.
    #[cfg(test)]
    pub(super) fn with_exhausted_epoch(self) -> Self {
        Self {
            epoch: u64::MAX,
            ..self
        }
    }

    /// Test-only: the same lineage pinned to the last representable policy
    /// revision.
    #[cfg(test)]
    pub(super) fn with_exhausted_policy(self) -> Self {
        Self {
            policy_revision: u64::MAX,
            ..self
        }
    }
}

/// The durable projection of an authority lineage.
///
/// Persisted under the orchestration store root so a restarted host keeps its
/// lineage instead of silently becoming a different authority that old records
/// would then be re-attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableAuthority {
    pub authority: Uuid,
    pub epoch: u64,
    pub policy_revision: u64,
}

impl DurableAuthority {
    pub(super) fn into_generation(self) -> AuthGeneration {
        AuthGeneration::adopt(self.authority, self.epoch, self.policy_revision)
    }
}

/// How a host established its authority lineage at startup.
///
/// Surfaced so an operator can tell a clean restart from a fail-closed
/// re-establishment; the two have very different consequences for records
/// written by the previous host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityOrigin {
    /// No durable record existed: a first-run lineage was minted.
    Fresh,
    /// A durable record was adopted and its epoch advanced past every identity
    /// the previous process had issued.
    Resumed,
    /// A durable record existed but could not be read or trusted. A brand-new
    /// lineage was minted, which invalidates every previously issued identity
    /// and quarantines every record stamped by the unreadable lineage.
    ReestablishedFailClosed,
}

impl AuthorityOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Resumed => "resumed",
            Self::ReestablishedFailClosed => "reestablished_fail_closed",
        }
    }
}

/// Authentication epoch and policy revision a durable record was stamped under.
///
/// Recorded for audit and diagnosis. Never consulted to grant access.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrincipalProvenance {
    pub epoch: u64,
    pub policy_revision: u64,
}

// ── credentials ─────────────────────────────────────────────────────────────

/// One named bearer credential accepted by a host instance.
///
/// The token stays private so accidental debug/JSON output cannot expose host
/// secrets. `incarnation` is minted fresh on every construction: re-registering
/// a credential id that was previously removed produces a *different*
/// credential, so identities and work bound to the removed one can never be
/// revived by reusing its id.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthCredential {
    id: String,
    incarnation: Uuid,
    token: String,
}

impl std::fmt::Debug for AuthCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthCredential")
            .field("id", &self.id)
            .field("incarnation", &self.incarnation)
            .field("token", &"[redacted]")
            .finish()
    }
}

impl AuthCredential {
    pub fn new(id: impl Into<String>, token: impl Into<String>) -> Result<Self, OrchError> {
        let id = id.into().trim().to_string();
        let token = token.into().trim().to_string();
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "auth credential id must contain only ASCII letters, numbers, '-', '_', or '.'",
            ));
        }
        if token.is_empty() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "auth credential token must not be empty",
            ));
        }
        Ok(Self {
            id,
            incarnation: Uuid::new_v4(),
            token,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// This registration of the credential id. Not stable across remove/re-add.
    pub(super) fn incarnation(&self) -> Uuid {
        self.incarnation
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// The public principal alias this credential authenticates as.
    fn principal_alias(&self) -> String {
        principal_alias(&self.id)
    }
}

/// Deterministic incarnation for a host-internal principal.
///
/// Derived from the lineage and epoch rather than random so the same internal
/// principal is stable within a generation, and different across generations:
/// a rotation gives the internal principal a new incarnation exactly as it does
/// a client credential.
fn internal_incarnation(principal: &str, generation: AuthGeneration) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(b"grokptah-internal-incarnation-v1\0");
    digest.update(generation.authority.as_bytes());
    digest.update(generation.epoch.to_be_bytes());
    digest.update(principal.as_bytes());
    let out = digest.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&out[..16]);
    Uuid::from_bytes(bytes)
}

/// The single definition of credential-id to wire-principal normalization.
///
/// The compatibility `primary` credential keeps emitting the established `mcp`
/// wire value; every other named device credential is its own principal. Run
/// stamping, run ownership and queue provenance all go through here so they can
/// never drift apart.
pub(super) fn principal_alias(credential_id: &str) -> String {
    if credential_id == "primary" {
        COMPAT_PRIMARY_PRINCIPAL.to_string()
    } else {
        credential_id.to_string()
    }
}

// ── principal kinds ─────────────────────────────────────────────────────────

/// What sort of authority an [`AuthContext`] carries.
///
/// Kept explicit so a service-internal identity can never be presented as, or
/// mistaken for, an authenticated remote client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalKind {
    /// Authenticated by presenting a bearer credential.
    Client,
    /// A host-internal worker that authenticates by construction. Still stamped
    /// with the live generation, so rotation invalidates it exactly as it does
    /// a client.
    ServiceInternal,
}

impl PrincipalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::ServiceInternal => "service_internal",
        }
    }
}

// ── delegation ──────────────────────────────────────────────────────────────

/// What a delegated identity is permitted to do relative to its delegator.
///
/// Deliberately a closed, narrow set. Fine-grained capability limiting belongs
/// to the capability-generation authority (#458); this enum is the typed seam
/// that work will attach to, and it is written so that adding a variant can
/// only ever *narrow* — there is no "everything the delegator can do" variant,
/// because a delegation that widened or even merely mirrored full authority
/// would defeat the point of delegating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationLimit {
    /// Reads only, within the delegator's own already-authorized scope.
    ReadOnlyWithinScope,
}

impl DelegationLimit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnlyWithinScope => "read_only_within_scope",
        }
    }

    /// Whether this limit permits an effect (non-read) boundary. Always false
    /// today; the match is exhaustive so a future variant must decide.
    pub fn permits_effects(self) -> bool {
        match self {
            Self::ReadOnlyWithinScope => false,
        }
    }
}

/// An explicit, expiring, revision-bound grant from one principal to another.
///
/// Minted only by the host, only from an already-current delegator identity,
/// and only for a bounded lifetime. It cannot widen authority: the delegate
/// inherits the delegator's owner, credential incarnation and generation
/// unchanged, and additionally carries a [`DelegationLimit`] that only narrows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delegation {
    id: Uuid,
    delegator: String,
    delegate: String,
    limit: DelegationLimit,
    expires_at: DateTime<Utc>,
    /// Generation the grant was minted under. A rotation invalidates the
    /// delegation exactly as it invalidates the delegator's own identity.
    generation: AuthGeneration,
}

impl Delegation {
    pub(super) fn mint(
        delegator: &AuthContext,
        delegate: impl Into<String>,
        limit: DelegationLimit,
        ttl_seconds: i64,
        now: DateTime<Utc>,
    ) -> Result<Self, OrchError> {
        let delegate = delegate.into().trim().to_string();
        if delegate.is_empty() || delegate.len() > 128 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "delegate principal must be between 1 and 128 bytes",
            ));
        }
        if delegate == delegator.principal {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "delegation must name a principal other than the delegator",
            ));
        }
        if delegator.delegation.is_some() {
            // Re-delegation is how a bounded grant quietly becomes an unbounded
            // one: each hop would reset the clock.
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "a delegated identity may not delegate further",
            ));
        }
        if ttl_seconds <= 0 || ttl_seconds > MAX_DELEGATION_TTL_SECONDS {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                format!(
                    "delegation ttl must be between 1 and {MAX_DELEGATION_TTL_SECONDS} seconds"
                ),
            ));
        }
        Ok(Self {
            id: Uuid::new_v4(),
            delegator: delegator.principal.clone(),
            delegate,
            limit,
            expires_at: now + ChronoDuration::seconds(ttl_seconds),
            generation: delegator.generation,
        })
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn delegator(&self) -> &str {
        &self.delegator
    }

    pub fn delegate(&self) -> &str {
        &self.delegate
    }

    pub fn limit(&self) -> DelegationLimit {
        self.limit
    }

    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    fn expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

// ── the authenticated caller ────────────────────────────────────────────────

/// One authenticated caller, as issued by the host.
///
/// Every field is private and there is no public constructor: a context can
/// only come from `OrchestrationService::auth_header` (or the host's own
/// internal minting), and it carries the generation that host was on when it
/// issued the context. Callers cannot fabricate an owner, a credential, a
/// tenant, a principal alias, a delegation, or a generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    kind: PrincipalKind,
    /// Stable credential identity used for audit and attribution. Not the
    /// secret, and safe to place in durable records.
    credential_id: String,
    /// *This registration* of `credential_id`. Removing and re-adding an id
    /// yields a different incarnation, so a reused id cannot inherit the
    /// removed credential's identities or work.
    credential_incarnation: Uuid,
    /// Account/tenant identity the credential authenticates within.
    owner_id: String,
    /// Canonical public principal alias. This — not `credential_id` — is what
    /// durable records are stamped with and what ownership is checked against.
    principal: String,
    generation: AuthGeneration,
    delegation: Option<Delegation>,
}

impl AuthContext {
    /// Issue a context for a caller that presented `credential`.
    pub(super) fn issue_for_credential(
        credential: &AuthCredential,
        owner_id: &str,
        generation: AuthGeneration,
    ) -> Self {
        Self {
            kind: PrincipalKind::Client,
            credential_id: credential.id().to_string(),
            credential_incarnation: credential.incarnation(),
            owner_id: owner_id.trim().to_string(),
            principal: credential.principal_alias(),
            generation,
            delegation: None,
        }
    }

    /// Issue a context for a host-internal worker that authenticates by
    /// construction rather than by bearer token.
    ///
    /// Not a general-purpose escape hatch: it is stamped with the live
    /// generation like any other context, it is marked
    /// [`PrincipalKind::ServiceInternal`], and its incarnation is derived from
    /// the generation so a rotation gives it a new one too.
    pub(super) fn issue_internal(
        principal: &str,
        owner_id: &str,
        generation: AuthGeneration,
    ) -> Self {
        Self {
            kind: PrincipalKind::ServiceInternal,
            credential_id: principal.to_string(),
            credential_incarnation: internal_incarnation(principal, generation),
            owner_id: owner_id.trim().to_string(),
            principal: principal.to_string(),
            generation,
            delegation: None,
        }
    }

    /// Derive the delegate's own identity from a minted grant.
    ///
    /// The delegate inherits owner, incarnation and generation unchanged — it
    /// cannot reach anything the delegator could not — and additionally carries
    /// the grant, whose limit and expiry are checked on every boundary.
    pub(super) fn delegated(&self, delegation: Delegation) -> Self {
        Self {
            kind: self.kind,
            credential_id: self.credential_id.clone(),
            credential_incarnation: self.credential_incarnation,
            owner_id: self.owner_id.clone(),
            principal: delegation.delegate.clone(),
            generation: self.generation,
            delegation: Some(delegation),
        }
    }

    pub fn kind(&self) -> PrincipalKind {
        self.kind
    }

    pub fn credential_id(&self) -> &str {
        &self.credential_id
    }

    pub(super) fn credential_incarnation(&self) -> Uuid {
        self.credential_incarnation
    }

    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    /// The public principal alias durable records are stamped with.
    pub fn principal(&self) -> &str {
        &self.principal
    }

    pub fn generation(&self) -> AuthGeneration {
        self.generation
    }

    pub fn delegation(&self) -> Option<&Delegation> {
        self.delegation.as_ref()
    }

    /// Whether this identity may cross an effect (mutating) boundary.
    pub(super) fn permits_effects(&self) -> bool {
        self.delegation
            .as_ref()
            .map(|d| d.limit.permits_effects())
            .unwrap_or(true)
    }

    /// Reject an identity whose delegation has run out.
    pub(super) fn check_delegation_window(&self, now: DateTime<Utc>) -> Result<(), OrchError> {
        match self.delegation.as_ref() {
            Some(delegation) if delegation.expired(now) => Err(OrchError::new(
                OrchErrorCode::Unauthenticated,
                "delegation has expired; request a new grant",
            )),
            _ => Ok(()),
        }
    }
}

// ── the authorized caller ───────────────────────────────────────────────────

/// One authenticated caller that has additionally been authorized for an exact
/// session and canonical workspace.
///
/// This is what read and effect boundaries that touch stored resources take.
/// It exists so "authenticated" can never be silently reused as "authorized
/// here": the session and workspace are bound into the value by the host at the
/// moment the scope check passed, not carried alongside it as parameters a
/// later call could swap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPrincipal {
    auth: AuthContext,
    session_id: Uuid,
    workspace: PathBuf,
    workspace_alias: String,
}

impl VerifiedPrincipal {
    /// Bind an authenticated caller to a scope the host has just verified.
    ///
    /// `workspace` must be the canonical path the scope check returned, never a
    /// caller-supplied string: a binding against an unverified path would
    /// authorize the caller for a workspace nobody checked.
    pub(super) fn bind(auth: &AuthContext, session_id: Uuid, workspace: &Path) -> Self {
        Self {
            auth: auth.clone(),
            session_id,
            workspace: workspace.to_path_buf(),
            workspace_alias: workspace_alias(workspace),
        }
    }

    pub fn auth(&self) -> &AuthContext {
        &self.auth
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    /// Canonical workspace path. Crate-internal: native paths never leave the
    /// host through a principal value.
    pub(super) fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Stable public alias for the workspace, safe to project and to persist.
    pub fn workspace_alias(&self) -> &str {
        &self.workspace_alias
    }

    pub fn principal(&self) -> &str {
        self.auth.principal()
    }

    pub fn owner_id(&self) -> &str {
        self.auth.owner_id()
    }

    pub fn generation(&self) -> AuthGeneration {
        self.auth.generation()
    }

    /// The opaque per-principal namespace this binding addresses.
    pub fn scope(&self) -> PrincipalScope {
        PrincipalScope::of(self)
    }
}

/// Stable public alias for a canonical workspace path.
///
/// A digest rather than the path: control-plane consumers get a value they can
/// compare and persist without the host leaking a native filesystem layout.
/// It is deliberately *not* a capability — the digest is unkeyed over a
/// low-entropy input, so a holder of a candidate path can confirm it offline.
/// Nothing is granted by presenting one.
pub(super) fn workspace_alias(workspace: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(b"grokptah-workspace-alias-v1\0");
    digest.update(workspace.to_string_lossy().as_bytes());
    format!("ws1-{:x}", digest.finalize())
}

/// The opaque per-principal namespace a binding addresses.
///
/// Used to keep one principal's idempotency receipts, queue entries and
/// per-caller records from colliding with another's. Like the workspace alias
/// it is an identifier, not a capability: it is derived, comparable and
/// persistable, and holding one grants nothing.
///
/// It binds owner, principal alias, credential incarnation, session and
/// workspace — but deliberately *not* the epoch, so a caller's own namespace
/// survives an unrelated rotation instead of orphaning its receipts. Freshness
/// comes from requiring a current identity to reach the namespace at all.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrincipalScope(String);

impl PrincipalScope {
    pub(super) fn of(principal: &VerifiedPrincipal) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"grokptah-principal-scope-v1\0");
        for part in [
            principal.auth.owner_id.as_str(),
            principal.auth.principal.as_str(),
            &principal.auth.credential_incarnation().to_string(),
            &principal.session_id.to_string(),
            principal.workspace_alias.as_str(),
        ] {
            digest.update(part.as_bytes());
            digest.update([0u8]);
        }
        Self(format!("ps1-{:x}", digest.finalize()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PrincipalScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── workspace policy ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct WorkspaceAllowlist {
    roots: Vec<PathBuf>,
}

impl WorkspaceAllowlist {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            roots: roots
                .into_iter()
                .filter_map(|p| canonical_workspace(&p).ok())
                .collect(),
        }
    }

    pub fn contains(&self, workspace: &Path) -> bool {
        let Ok(c) = canonical_workspace(workspace) else {
            return false;
        };
        self.roots.iter().any(|r| r == &c)
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
}

/// Canonical absolute path for workspace identity comparisons.
pub fn canonical_workspace(path: &Path) -> Result<PathBuf, OrchError> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?
            .join(path)
    };
    dunce::canonicalize(&abs).map_err(|e| {
        OrchError::new(
            OrchErrorCode::WorkspaceMismatch,
            format!("cannot canonicalize {}: {e}", abs.display()),
        )
    })
}

/// Constant-time equality for equal-length secrets; length mismatch is not constant-time
/// (still fail closed without short-circuiting byte compares of equal lengths).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── authentication ──────────────────────────────────────────────────────────

/// Authenticate a bearer header against `credentials` and stamp the resulting
/// context with `generation`.
///
/// The caller supplies the generation because only the issuing host knows its
/// own authority; it must pass the generation it read *before* reading
/// `credentials`, so a rotation racing this call yields a context that is
/// already stale rather than one that is current under freshly rotated
/// credentials.
///
/// Every credential is compared even after a match so the work done is a
/// function of the credential count alone, not of which credential matched or
/// whether any did.
pub(super) fn authenticate_bearer(
    header: Option<&str>,
    credentials: &[AuthCredential],
    owner_id: &str,
    generation: AuthGeneration,
) -> Result<AuthContext, OrchError> {
    if credentials.is_empty() || owner_id.trim().is_empty() {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "control plane credentials are not configured",
        ));
    }
    let Some(h) = header else {
        return Err(OrchError::new(
            OrchErrorCode::Unauthenticated,
            "missing Authorization bearer token",
        ));
    };
    let h = h.trim();
    let token = h
        .strip_prefix("Bearer ")
        .or_else(|| h.strip_prefix("bearer "))
        .unwrap_or("")
        .trim();
    if token.is_empty() {
        return Err(OrchError::new(
            OrchErrorCode::Unauthenticated,
            "missing bearer token",
        ));
    }
    let mut matched: Option<&AuthCredential> = None;
    for credential in credentials {
        if constant_time_eq(token.as_bytes(), credential.token.as_bytes()) && matched.is_none() {
            matched = Some(credential);
        }
    }
    let Some(credential) = matched else {
        return Err(OrchError::new(
            OrchErrorCode::Unauthenticated,
            "invalid bearer token",
        ));
    };
    Ok(AuthContext::issue_for_credential(
        credential,
        owner_id.trim(),
        generation,
    ))
}

pub fn require_workspace_match(
    allowlist: &WorkspaceAllowlist,
    session_cwd: Option<&Path>,
    claimed: &Path,
) -> Result<PathBuf, OrchError> {
    let claimed_c = canonical_workspace(claimed)?;
    if !allowlist.contains(&claimed_c) {
        return Err(OrchError::new(
            OrchErrorCode::WorkspaceMismatch,
            "workspace not in allowlist",
        ));
    }
    let Some(scwd) = session_cwd else {
        return Err(OrchError::new(
            OrchErrorCode::WorkspaceMismatch,
            "session has no project cwd",
        ));
    };
    let session_c = canonical_workspace(scwd)?;
    if session_c != claimed_c {
        return Err(OrchError::new(
            OrchErrorCode::WorkspaceMismatch,
            "session workspace does not match claimed workspace",
        ));
    }
    Ok(claimed_c)
}

// ── narrow internal authority ───────────────────────────────────────────────

/// Authority for the host's own unauthenticated readiness probe.
///
/// Deliberately *not* an [`AuthContext`]. The readiness path used to fabricate
/// a public context with invented `token_id`/`owner_id` values, which meant an
/// unauthenticated local probe held a value indistinguishable from a real
/// caller's. This type carries no principal, no owner and no generation; it
/// unlocks exactly one narrow capacity read and nothing else, and it cannot be
/// passed anywhere an `AuthContext` is expected.
///
/// It has no public constructor, so an embedder cannot mint one either.
#[derive(Debug, Clone, Copy)]
pub struct ReadinessAuthority {
    _private: (),
}

impl ReadinessAuthority {
    pub(crate) fn internal() -> Self {
        Self { _private: () }
    }
}

#[cfg(test)]
mod fence_shape {
    //! Mechanical proof that the identity types cannot be constructed by a
    //! caller.
    //!
    //! Rust's own privacy rules are the real fence; this reads the module's
    //! source so that *weakening* them is a test failure rather than a silent
    //! change. A `pub` field on any identity type would let a holder of a
    //! legitimate identity rewrite its owner or principal, and a public
    //! constructor would let a caller assert an identity outright — both are
    //! exactly the holes #477 exists to close, and neither produces a
    //! compile error anywhere on its own.

    /// This file minus this module and the behaviour tests below it, so the
    /// assertions cannot match the string literals they are written with.
    fn fence_source() -> &'static str {
        let src = include_str!("authz.rs");
        let cut = src
            .find("mod fence_shape")
            .expect("this module is declared in this file");
        &src[..cut]
    }

    /// Every type whose construction must stay host-issued.
    const IDENTITY_TYPES: &[&str] = &[
        "AuthGeneration",
        "AuthContext",
        "VerifiedPrincipal",
        "PrincipalScope",
        "AuthCredential",
        "Delegation",
        "ReadinessAuthority",
    ];

    /// The field list of `name`, with the declaration itself removed so the
    /// scan sees only what a caller could write to.
    fn struct_fields(src: &str, name: &str) -> String {
        let decl = format!("pub struct {name}");
        let start = src
            .find(&decl)
            .unwrap_or_else(|| panic!("{name} must be declared in the fence"));
        let rest = &src[start + decl.len()..];
        let brace = rest.find('{');
        let semi = rest.find(';');
        match (brace, semi) {
            // Tuple struct: the fields are inside the parentheses.
            (brace, Some(sc)) if brace.is_none_or(|b| sc < b) => rest[..sc].to_string(),
            (Some(b), _) => {
                let end = rest[b..].find('}').expect("struct body must close") + b;
                rest[b + 1..end].to_string()
            }
            _ => panic!("{name} declaration is not parseable"),
        }
    }

    #[test]
    fn identity_types_have_no_publicly_writable_fields() {
        let src = fence_source();
        for name in IDENTITY_TYPES {
            let fields = struct_fields(src, name);
            for line in fields.split([',', '\n']) {
                let line = line.trim().trim_start_matches('(');
                assert!(
                    !line.starts_with("pub "),
                    "{name} exposes a writable field `{line}`; a holder of a legitimate \
                     identity could then rewrite it and impersonate another principal"
                );
            }
        }
    }

    #[test]
    fn identity_minting_is_never_public() {
        let src = fence_source();
        // Constructors and mutators that would let a caller assert, widen or
        // rewind an identity. Each must be private or `pub(super)`.
        const MINTERS: &[&str] = &[
            "fn new_authority",
            "fn adopt",
            "fn next_epoch",
            "fn next_policy",
            "fn to_durable",
            "fn issue_for_credential",
            "fn issue_internal",
            "fn delegated",
            "fn bind",
            "fn of",
            "fn mint",
            "fn authenticate_bearer",
            "fn internal",
        ];
        for minter in MINTERS {
            let mut found = false;
            for line in src.lines() {
                let line = line.trim();
                if !line.contains(minter) {
                    continue;
                }
                found = true;
                assert!(
                    !line.starts_with("pub fn"),
                    "`{minter}` is publicly callable: `{line}`. Identity is issued, \
                     never asserted, so every minting path must stay inside the fence"
                );
            }
            assert!(
                found,
                "`{minter}` disappeared; update this list deliberately"
            );
        }
    }

    #[test]
    fn accessors_never_hand_out_the_lineage_or_a_secret() {
        let src = fence_source();
        // `authority` is the lineage id and `token` is bearer material. Neither
        // may leave through a public accessor: the lineage id would let a
        // caller reconstruct a generation, and the token is the secret itself.
        for banned in ["pub fn authority(", "pub fn token_of(", "pub fn secret("] {
            assert!(
                !src.contains(banned),
                "`{banned}` would leak host-private material out of the fence"
            );
        }
        assert!(
            src.contains("pub fn token(&self)"),
            "AuthCredential::token is the embedder's own configured secret and stays \
             readable; if that changes, update this test deliberately"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn credential(id: &str, token: &str) -> AuthCredential {
        AuthCredential::new(id, token).unwrap()
    }

    #[test]
    fn named_credentials_return_principal_alias_and_shared_owner() {
        let credentials = vec![credential("primary", "tok"), credential("laptop", "other")];
        let generation = AuthGeneration::new_authority();
        let auth = authenticate_bearer(Some("Bearer other"), &credentials, "account-1", generation)
            .unwrap();
        assert_eq!(auth.credential_id(), "laptop");
        assert_eq!(auth.principal(), "laptop");
        assert_eq!(auth.owner_id(), "account-1");
        assert_eq!(auth.generation(), generation);
        assert_eq!(auth.kind(), PrincipalKind::Client);

        let compat =
            authenticate_bearer(Some("Bearer tok"), &credentials, "account-1", generation).unwrap();
        assert_eq!(compat.credential_id(), "primary");
        assert_eq!(
            compat.principal(),
            COMPAT_PRIMARY_PRINCIPAL,
            "the compatibility credential must keep emitting the established wire value"
        );

        assert!(authenticate_bearer(
            Some("Bearer unknown"),
            &credentials,
            "account-1",
            generation
        )
        .is_err());
    }

    #[test]
    fn epoch_and_policy_advance_independently_and_fail_closed_on_overflow() {
        let first = AuthGeneration::new_authority();
        assert_eq!((first.epoch(), first.policy_revision()), (0, 0));

        let credential_rotation = first.next_epoch().unwrap();
        assert_eq!(
            (
                credential_rotation.epoch(),
                credential_rotation.policy_revision()
            ),
            (1, 0),
            "a credential rotation advances the epoch but not the policy revision"
        );
        assert_ne!(first, credential_rotation);

        let policy_rotation = credential_rotation.next_policy().unwrap();
        assert_eq!(
            (policy_rotation.epoch(), policy_rotation.policy_revision()),
            (2, 1),
            "a policy rotation advances both"
        );

        let err = first.with_exhausted_epoch().next_epoch().unwrap_err();
        assert_eq!(err.code, OrchErrorCode::Internal);
        assert!(err.message.contains("generation exhausted"));
        assert!(first.with_exhausted_epoch().next_policy().is_err());
        assert!(first.with_exhausted_policy().next_policy().is_err());
        assert!(
            first.with_exhausted_policy().next_epoch().is_ok(),
            "an exhausted policy revision must not block a pure credential rotation"
        );
    }

    #[test]
    fn distinct_authorities_never_compare_equal_but_adoption_round_trips() {
        let a = AuthGeneration::new_authority();
        let b = AuthGeneration::new_authority();
        assert_ne!(a, b, "each lineage must be unique to its host");

        let durable = a.next_epoch().unwrap().to_durable();
        let resumed = durable.into_generation();
        assert_eq!(
            resumed,
            a.next_epoch().unwrap(),
            "adoption must reproduce the persisted lineage exactly"
        );
        assert_ne!(resumed, a, "the persisted epoch is not the original epoch");
    }

    #[test]
    fn credential_incarnation_changes_on_re_add() {
        let first = credential("laptop", "tok");
        let re_added = credential("laptop", "tok");
        assert_eq!(first.id(), re_added.id());
        assert_ne!(
            first.incarnation(),
            re_added.incarnation(),
            "re-registering a credential id must not inherit the removed one's incarnation"
        );

        let generation = AuthGeneration::new_authority();
        let before = AuthContext::issue_for_credential(&first, "acct", generation);
        let after = AuthContext::issue_for_credential(&re_added, "acct", generation);
        assert_ne!(
            before, after,
            "identities from two incarnations of one id must not be interchangeable"
        );
    }

    #[test]
    fn internal_principals_are_marked_and_regenerate_on_rotation() {
        let generation = AuthGeneration::new_authority();
        let executor = AuthContext::issue_internal(NATIVE_EXECUTOR_PRINCIPAL, "acct", generation);
        assert_eq!(executor.kind(), PrincipalKind::ServiceInternal);
        assert_eq!(executor.principal(), NATIVE_EXECUTOR_PRINCIPAL);

        let rotated = AuthContext::issue_internal(
            NATIVE_EXECUTOR_PRINCIPAL,
            "acct",
            generation.next_epoch().unwrap(),
        );
        assert_ne!(
            executor.credential_incarnation(),
            rotated.credential_incarnation(),
            "rotation must give the internal principal a new incarnation too"
        );
    }

    #[test]
    fn delegation_is_expiring_bounded_and_cannot_widen() {
        let generation = AuthGeneration::new_authority();
        let delegator =
            AuthContext::issue_for_credential(&credential("laptop", "tok"), "acct", generation);
        let now = Utc::now();

        assert!(
            Delegation::mint(
                &delegator,
                "helper",
                DelegationLimit::ReadOnlyWithinScope,
                0,
                now
            )
            .is_err(),
            "a zero ttl is not a delegation"
        );
        assert!(
            Delegation::mint(
                &delegator,
                "helper",
                DelegationLimit::ReadOnlyWithinScope,
                MAX_DELEGATION_TTL_SECONDS + 1,
                now,
            )
            .is_err(),
            "ttl must be bounded"
        );
        assert!(
            Delegation::mint(
                &delegator,
                delegator.principal(),
                DelegationLimit::ReadOnlyWithinScope,
                60,
                now,
            )
            .is_err(),
            "delegating to yourself is not a narrowing"
        );

        let grant = Delegation::mint(
            &delegator,
            "helper",
            DelegationLimit::ReadOnlyWithinScope,
            60,
            now,
        )
        .unwrap();
        let delegate = delegator.delegated(grant);
        assert_eq!(delegate.principal(), "helper");
        assert_eq!(
            delegate.owner_id(),
            delegator.owner_id(),
            "a delegation must not cross tenants"
        );
        assert_eq!(
            delegate.generation(),
            delegator.generation(),
            "a delegation is bound to the generation it was minted under"
        );
        assert!(
            !delegate.permits_effects(),
            "a read-only delegation must not cross an effect boundary"
        );
        assert!(delegator.permits_effects());

        assert!(delegate.check_delegation_window(now).is_ok());
        assert!(
            delegate
                .check_delegation_window(now + ChronoDuration::seconds(61))
                .is_err(),
            "an expired delegation must fail closed"
        );

        assert!(
            Delegation::mint(
                &delegate,
                "third",
                DelegationLimit::ReadOnlyWithinScope,
                60,
                now
            )
            .is_err(),
            "re-delegation would reset the expiry clock"
        );
    }

    #[test]
    fn principal_scope_separates_principals_sessions_and_workspaces() {
        let generation = AuthGeneration::new_authority();
        let one = AuthContext::issue_for_credential(&credential("laptop", "a"), "acct", generation);
        let two = AuthContext::issue_for_credential(&credential("phone", "b"), "acct", generation);
        let session = Uuid::new_v4();
        let ws = PathBuf::from("/w");
        let other_ws = PathBuf::from("/other");

        let base = VerifiedPrincipal::bind(&one, session, &ws);
        assert_eq!(
            base.scope(),
            VerifiedPrincipal::bind(&one, session, &ws).scope()
        );
        assert_ne!(
            base.scope(),
            VerifiedPrincipal::bind(&two, session, &ws).scope()
        );
        assert_ne!(
            base.scope(),
            VerifiedPrincipal::bind(&one, Uuid::new_v4(), &ws).scope()
        );
        assert_ne!(
            base.scope(),
            VerifiedPrincipal::bind(&one, session, &other_ws).scope()
        );

        assert!(
            base.workspace_alias().starts_with("ws1-"),
            "the public alias must not be a native path: {}",
            base.workspace_alias()
        );
        assert!(!base.workspace_alias().contains('/'));
        assert!(base.scope().as_str().starts_with("ps1-"));
    }

    #[test]
    fn scope_survives_rotation_but_identity_does_not() {
        let generation = AuthGeneration::new_authority();
        let cred = credential("laptop", "a");
        let session = Uuid::new_v4();
        let ws = PathBuf::from("/w");

        let before = AuthContext::issue_for_credential(&cred, "acct", generation);
        let after =
            AuthContext::issue_for_credential(&cred, "acct", generation.next_policy().unwrap());
        assert_ne!(
            before.generation(),
            after.generation(),
            "rotation must invalidate the previously issued identity"
        );
        assert_eq!(
            VerifiedPrincipal::bind(&before, session, &ws).scope(),
            VerifiedPrincipal::bind(&after, session, &ws).scope(),
            "a caller's own namespace must survive an unrelated rotation"
        );
    }

    #[test]
    fn provenance_records_generation_without_granting() {
        let generation = AuthGeneration::new_authority()
            .next_policy()
            .unwrap()
            .next_epoch()
            .unwrap();
        assert_eq!(
            generation.provenance(),
            PrincipalProvenance {
                epoch: 2,
                policy_revision: 1,
            }
        );
    }

    #[test]
    fn allowlist_canonical() {
        let d = tempdir().unwrap();
        let root = d.path().to_path_buf();
        let al = WorkspaceAllowlist::new([root.clone()]);
        assert!(al.contains(&root));
        let other = tempdir().unwrap();
        assert!(!al.contains(other.path()));
    }

    #[test]
    fn allowlist_symlink_into_root_accepted_outside_rejected() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let al = WorkspaceAllowlist::new([root.path().to_path_buf()]);

        // Symlink whose target resolves inside the allowlisted root → allowed.
        let link_in = root.path().join("via-link");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path(), &link_in).unwrap();
            assert!(
                al.contains(&link_in),
                "symlink canonicalizing into allowlisted root must be accepted"
            );
        }

        // Symlink whose target is outside the allowlist → fail closed.
        let link_out = root.path().join("escape");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), &link_out).unwrap();
            assert!(
                !al.contains(&link_out),
                "symlink escaping allowlisted root must be rejected"
            );
            // require_workspace_match also fails closed on escaped claim.
            let err = require_workspace_match(&al, Some(root.path()), &link_out);
            assert!(err.is_err());
        }

        // Path that canonicalizes outside the allowlisted root is rejected.
        assert!(!al.contains(outside.path()));
    }
}
