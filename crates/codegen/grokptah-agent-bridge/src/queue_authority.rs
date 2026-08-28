//! Canonical principal ownership for session prompt queues (#461).
//!
//! # Why this exists
//!
//! Queue entries carried a free-form `owner` string that was hardcoded to
//! `mcp` for every control-plane caller and `desktop` for the local UI. Two
//! distinct authenticated MCP principals therefore shared one owner value, so
//! the queue could not prove that one principal may not read, edit, reorder,
//! steer, execute, or remove another principal's queued work.
//!
//! # The model
//!
//! Ownership is one opaque, non-reversible digest — the [`QueueOwnerKey`] —
//! over the tuple that actually decides authority:
//!
//! * **tenant** — the account the credential maps to (`AuthContext::owner_id`),
//! * **principal** — the wire principal (`desktop`, `mcp`, or a named device
//!   credential id),
//! * **session** — the exact session the queue belongs to,
//! * **workspace** — the canonical workspace path.
//!
//! The digest is what gets compared and what gets persisted. It is **not** a
//! capability and is not projected to control-plane consumers: the digest is
//! unkeyed over low-entropy inputs, so a holder of a candidate tuple can
//! confirm it offline. Opaque projectable handles need a durable host-held key
//! and are out of scope here.
//!
//! # Authority vs. provenance
//!
//! Two things are deliberately kept apart:
//!
//! * The **ownership key** is stable across process restart and across bearer
//!   token rotation, because none of its inputs change when a secret is
//!   re-minted for the same credential id. It answers "whose entry is this".
//! * The **provenance** ([`QueueProvenance`]) records the authentication epoch
//!   and policy revision the entry was stamped under. It is recorded, audited,
//!   and surfaced — but it never grants anything on its own.
//!
//! Making the authority id part of the ownership key was considered and
//! rejected: a fresh authority is minted per service instance, so it would
//! orphan every persisted entry on every restart. Restart safety instead comes
//! from requiring a *current* authentication context to touch the queue at all,
//! plus the delivery-time revalidation below.
//!
//! # Revocation
//!
//! [`QueueAuthority`] is the live authorization snapshot the orchestration
//! service publishes into the host. Because the ownership key binds the
//! workspace, and the control plane already requires the claimed workspace to
//! equal the session's own cwd, the host can *recompute* the set of currently
//! authorized keys for a session from the live principal list alone. An entry
//! whose key is not in that set is withheld from delivery: removing a
//! credential or dropping a workspace out of the allowlist revokes queued
//! execution authority without deleting the entry, so audit evidence survives.

use std::collections::BTreeSet;
use std::path::Path;

use sha2::{Digest, Sha256};

/// The canonical workspace string an ownership key binds to.
///
/// Ownership must not fork just because two callers spelled the same directory
/// differently, so both the host and the control plane derive the workspace
/// component of a key from this one function. Canonicalization can fail for a
/// path that no longer exists; the lexical form is then used, which is stable
/// and still distinguishes different workspaces — it simply cannot merge two
/// spellings of a directory that is already gone.
pub fn workspace_key(path: &Path) -> String {
    dunce::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Wire principal for the local desktop UI.
///
/// The desktop is the host's own front end, not a remote caller: there is
/// exactly one of it per host process, so it is a single principal by
/// construction rather than by policy.
pub const DESKTOP_PRINCIPAL: &str = "desktop";

/// Wire principal for the compatibility control-plane credential.
///
/// This is the established value the `primary` credential has always emitted,
/// kept so existing receipts and event consumers do not change shape. It names
/// exactly one credential; it is *not* a shared bucket, and nothing in this
/// module treats it as one.
pub const CONTROL_PRINCIPAL: &str = "mcp";

/// Tenant used for host-local (desktop) ownership when no service account is
/// in play.
pub const LOCAL_TENANT: &str = "local";

/// Upper bound on the principals a single authority snapshot may carry.
///
/// Revalidation recomputes one candidate key per live principal, so this keeps
/// that work bounded no matter what an embedder installs.
const MAX_AUTHORIZED_PRINCIPALS: usize = 512;

/// Ownership handle: a digest of the identity tuple, used for comparison.
///
/// Rendered as `v1-sha256:<hex>`, matching the fingerprint idiom already used
/// for credential identity elsewhere in the crate.
///
/// **This is not an opaque capability and must not be projected as one.** The
/// digest is unkeyed and its inputs are low-entropy and often guessable — a
/// tenant, a wire principal, a session id, and an absolute workspace path — so
/// anyone holding a candidate tuple can confirm it offline. That makes a
/// projected handle a path oracle, not a secret. It is therefore used for
/// storage and equality only; a handle safe to hand to SDK/broker/browser
/// consumers needs a durable host-held key (HMAC or a sealed random id), which
/// is new durable authority state and is deliberately not invented here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueueOwnerKey(String);

impl QueueOwnerKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for QueueOwnerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Length-prefixed field mixing, so that distinct tuples cannot collide by
/// shifting a delimiter between adjacent fields.
fn mix(digest: &mut Sha256, label: &str, value: &str) {
    digest.update((label.len() as u64).to_be_bytes());
    digest.update(label.as_bytes());
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}

/// The identity that owns a queue entry.
///
/// Construct through [`QueuePrincipal::control`] or
/// [`QueuePrincipal::desktop`] so the tenant/principal pairing cannot be
/// assembled inconsistently at a call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuePrincipal {
    tenant: String,
    principal: String,
    session: String,
    workspace: String,
}

impl QueuePrincipal {
    /// Ownership for an authenticated control-plane caller.
    ///
    /// `principal` is the caller's wire principal, which the orchestration
    /// service derives from its single `client_principal` definition so run
    /// ownership and queue ownership cannot drift apart.
    pub fn control(
        tenant: impl Into<String>,
        principal: impl Into<String>,
        session: impl std::fmt::Display,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            tenant: tenant.into(),
            principal: principal.into(),
            session: session.to_string(),
            workspace: workspace.into(),
        }
    }

    /// Ownership for the local desktop UI.
    ///
    /// The desktop has no bearer credential and no service account, so it owns
    /// under the reserved local tenant. Workspace is still bound: a desktop
    /// entry queued against one project does not become owned when the same
    /// session id is later observed elsewhere.
    pub fn desktop(session: impl std::fmt::Display, workspace: impl Into<String>) -> Self {
        Self::control(LOCAL_TENANT, DESKTOP_PRINCIPAL, session, workspace)
    }

    /// The opaque ownership digest that is compared and persisted.
    pub fn key(&self) -> QueueOwnerKey {
        let mut digest = Sha256::new();
        digest.update(b"grokptah.queue.owner.v1");
        mix(&mut digest, "tenant", &self.tenant);
        mix(&mut digest, "principal", &self.principal);
        mix(&mut digest, "session", &self.session);
        mix(&mut digest, "workspace", &self.workspace);
        QueueOwnerKey(format!("v1-sha256:{:x}", digest.finalize()))
    }

    /// The wire principal, which is what receipts and events already carry.
    pub fn wire_principal(&self) -> &str {
        &self.principal
    }

    pub fn tenant(&self) -> &str {
        &self.tenant
    }
}

/// Authentication epoch and policy revision an entry was stamped under.
///
/// Recorded for audit and diagnosis. Never consulted to grant access: a higher
/// epoch does not make an entry more privileged, and a lower one does not make
/// it someone else's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QueueProvenance {
    /// Authentication epoch counter current when the entry was stamped.
    pub epoch: u64,
    /// Policy/capability revision current when the entry was stamped.
    pub policy: u64,
}

/// A caller presenting itself at a queue boundary.
///
/// Carries both the ownership identity and the provenance to stamp. Every host
/// queue entry point takes one of these in place of the old free-form `origin`
/// string, so there is no way to reach the state machine without naming an
/// owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueActor {
    principal: QueuePrincipal,
    provenance: QueueProvenance,
}

impl QueueActor {
    pub fn new(principal: QueuePrincipal, provenance: QueueProvenance) -> Self {
        Self {
            principal,
            provenance,
        }
    }

    /// Local desktop actor. Provenance is zero: the desktop authenticates by
    /// being in-process, not by an epoch-stamped bearer context.
    pub fn desktop(session: impl std::fmt::Display, workspace: impl Into<String>) -> Self {
        Self::new(
            QueuePrincipal::desktop(session, workspace),
            QueueProvenance::default(),
        )
    }

    pub fn key(&self) -> QueueOwnerKey {
        self.principal.key()
    }

    /// The value that lands in the `origin` field of receipts and events.
    pub fn origin(&self) -> &str {
        self.principal.wire_principal()
    }

    pub fn principal(&self) -> &QueuePrincipal {
        &self.principal
    }

    pub fn provenance(&self) -> QueueProvenance {
        self.provenance
    }

    /// Opaque per-principal namespace for idempotency receipts.
    ///
    /// Receipts were keyed by `request_id` alone, so one principal's
    /// `request_id` collided with another's: an exact-payload collision
    /// replayed the first principal's response (leaking its queue contents),
    /// and a differing payload returned a `request_id reused` conflict that
    /// confirmed the other principal had used that id. Namespacing by tenant
    /// and principal removes both without changing single-principal replay.
    ///
    /// Session and workspace are deliberately *not* mixed in: a request id is
    /// scoped to a principal, and including narrower dimensions would let the
    /// same id be replayed with different effects across sessions.
    pub fn idempotency_scope(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"grokptah.queue.idempotency.v1");
        mix(&mut digest, "tenant", &self.principal.tenant);
        mix(&mut digest, "principal", &self.principal.principal);
        format!("{:x}", digest.finalize())
    }
}

/// Why a queue authority snapshot could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueAuthorityError {
    /// More live principals than revalidation is willing to recompute. Failing
    /// closed keeps the previous authority rather than silently revoking the
    /// principals past the cap.
    TooManyPrincipals { count: usize, limit: usize },
}

impl std::fmt::Display for QueueAuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyPrincipals { count, limit } => write!(
                f,
                "queue authority carries {count} principals, above the {limit} supported"
            ),
        }
    }
}

impl std::error::Error for QueueAuthorityError {}

/// Live authorization snapshot published by the orchestration service into the
/// host.
///
/// The host owns the queue state machine but not the credential set, so the
/// service pushes what is currently authorized rather than the host reaching
/// back into the service. That keeps one authority: the service decides, the
/// host enforces, and there is no second auth system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueAuthority {
    tenant: String,
    principals: BTreeSet<String>,
    /// Canonical allowlisted workspace roots. `None` means no control plane is
    /// configured and no allowlist constraint applies — the desktop-only
    /// deployment, whose single local principal is authorized by construction.
    workspaces: Option<BTreeSet<String>>,
    provenance: QueueProvenance,
}

impl Default for QueueAuthority {
    /// Desktop-only default: the local UI is authorized, nothing else is, and
    /// no workspace allowlist applies.
    fn default() -> Self {
        Self {
            tenant: LOCAL_TENANT.into(),
            principals: BTreeSet::new(),
            workspaces: None,
            provenance: QueueProvenance::default(),
        }
    }
}

impl QueueAuthority {
    /// An authority that authorizes nobody.
    ///
    /// Installed for the duration of a credential/allowlist rotation. The
    /// service mutates its policy state and republishes in separate steps, and
    /// delivery reads the host's snapshot without consulting the service, so a
    /// drain landing between those steps would otherwise deliver under the
    /// pre-rotation authority. Quiescing makes that window fail closed in both
    /// directions: a rotation that narrows authority cannot leak the old width,
    /// and one that widens it cannot grant early. A drain during the window
    /// simply finds nothing deliverable and is retried.
    pub fn quiesced() -> Self {
        Self {
            tenant: String::new(),
            principals: BTreeSet::new(),
            // `Some(empty)` rather than `None`: an empty allowlist authorizes
            // nobody, whereas `None` means "no allowlist constraint" and would
            // leave the desktop principal deliverable.
            workspaces: Some(BTreeSet::new()),
            provenance: QueueProvenance::default(),
        }
    }

    /// Snapshot for a configured control plane.
    ///
    /// `principals` are the wire principals of the live credentials;
    /// `workspaces` are the canonical allowlisted roots. Both are the *current*
    /// values, so removing a credential or a root immediately narrows what may
    /// still be delivered.
    pub fn control(
        tenant: impl Into<String>,
        principals: impl IntoIterator<Item = String>,
        workspaces: impl IntoIterator<Item = String>,
        provenance: QueueProvenance,
    ) -> Result<Self, QueueAuthorityError> {
        let principals: BTreeSet<String> = principals.into_iter().collect();
        // Truncating here would silently revoke every principal past the cap:
        // their queued work would stop being delivered, with no error raised
        // anywhere. Refusing to build the snapshot leaves the previously
        // published authority in force and surfaces the misconfiguration.
        if principals.len() > MAX_AUTHORIZED_PRINCIPALS {
            return Err(QueueAuthorityError::TooManyPrincipals {
                count: principals.len(),
                limit: MAX_AUTHORIZED_PRINCIPALS,
            });
        }
        Ok(Self {
            tenant: tenant.into(),
            principals,
            workspaces: Some(workspaces.into_iter().collect()),
            provenance,
        })
    }

    pub fn provenance(&self) -> QueueProvenance {
        self.provenance
    }

    /// Whether `key` is still authorized to have work delivered for `session`
    /// running in `workspace`.
    ///
    /// The desktop principal is always a candidate: it is the host's own UI and
    /// is not revoked by control-plane credential changes. Every other
    /// candidate comes from the live principal list, so a removed credential
    /// stops matching immediately.
    ///
    /// A workspace that is no longer allowlisted authorizes nobody, which is
    /// how an allowlist change revokes queued execution authority.
    pub fn authorizes(
        &self,
        key: &QueueOwnerKey,
        session: impl std::fmt::Display,
        workspace: &str,
    ) -> bool {
        let session = session.to_string();
        if let Some(workspaces) = self.workspaces.as_ref() {
            if !workspaces.contains(workspace) {
                return false;
            }
        }
        if &QueuePrincipal::desktop(&session, workspace).key() == key {
            return true;
        }
        self.principals.iter().any(|principal| {
            &QueuePrincipal::control(&self.tenant, principal, &session, workspace).key() == key
        })
    }
}

/// A [`QueueAuthority`] bound to one session and workspace, ready to answer
/// delivery questions about individual entries.
///
/// Binding the session and workspace once, outside the queue lock, keeps the
/// per-entry check to a digest comparison and keeps the queue state machine
/// free of any knowledge of credentials.
#[derive(Debug, Clone)]
pub struct DeliveryGate<'a> {
    authority: &'a QueueAuthority,
    session: String,
    workspace: String,
}

impl<'a> DeliveryGate<'a> {
    pub fn new(
        authority: &'a QueueAuthority,
        session: impl std::fmt::Display,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            authority,
            session: session.to_string(),
            workspace: workspace.into(),
        }
    }

    /// Whether an entry stamped with `owner_key` may be delivered now.
    ///
    /// A quarantined entry (`None`) is never deliverable: it predates principal
    /// ownership, so running it would execute work on behalf of a principal the
    /// host cannot name.
    pub fn allows_owner(&self, owner_key: Option<&str>) -> bool {
        let Some(owner_key) = owner_key else {
            return false;
        };
        self.authority.authorizes(
            &QueueOwnerKey(owner_key.to_string()),
            &self.session,
            &self.workspace,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn session() -> Uuid {
        Uuid::from_u128(0x5e5510)
    }

    #[test]
    fn distinct_principals_never_share_an_ownership_key() {
        let a = QueuePrincipal::control("acct", "laptop", session(), "/w");
        let b = QueuePrincipal::control("acct", "ci-runner", session(), "/w");
        assert_ne!(a.key(), b.key());
    }

    #[test]
    fn ownership_binds_tenant_session_and_workspace_independently() {
        let base = QueuePrincipal::control("acct", "laptop", session(), "/w");
        let other_tenant = QueuePrincipal::control("acct-2", "laptop", session(), "/w");
        let other_session = QueuePrincipal::control("acct", "laptop", Uuid::from_u128(2), "/w");
        let other_workspace = QueuePrincipal::control("acct", "laptop", session(), "/other");
        for other in [other_tenant, other_session, other_workspace] {
            assert_ne!(
                base.key(),
                other.key(),
                "every ownership dimension must be load bearing"
            );
        }
    }

    #[test]
    fn ownership_key_is_stable_for_the_same_identity() {
        let a = QueuePrincipal::control("acct", "laptop", session(), "/w");
        let b = QueuePrincipal::control("acct", "laptop", session(), "/w");
        assert_eq!(
            a.key(),
            b.key(),
            "a rotated bearer token must not orphan a credential's own queue"
        );
    }

    #[test]
    fn field_boundaries_cannot_be_shifted_to_forge_a_key() {
        // Without length prefixing, ("ab","c") and ("a","bc") would hash the
        // same, letting a principal named to straddle the boundary impersonate
        // another.
        let left = QueuePrincipal::control("ab", "c", session(), "/w");
        let right = QueuePrincipal::control("a", "bc", session(), "/w");
        assert_ne!(left.key(), right.key());
    }

    #[test]
    fn key_never_discloses_its_inputs() {
        let key = QueuePrincipal::control("acct", "laptop", session(), "/secret/path").key();
        let rendered = key.to_string();
        assert!(rendered.starts_with("v1-sha256:"));
        for secret in ["acct", "laptop", "/secret/path"] {
            assert!(
                !rendered.contains(secret),
                "ownership handle must stay opaque, leaked {secret}"
            );
        }
    }

    #[test]
    fn idempotency_scope_separates_principals_but_not_sessions() {
        let a = QueueActor::desktop(session(), "/w");
        let laptop = QueueActor::new(
            QueuePrincipal::control("acct", "laptop", session(), "/w"),
            QueueProvenance::default(),
        );
        let ci = QueueActor::new(
            QueuePrincipal::control("acct", "ci", session(), "/w"),
            QueueProvenance::default(),
        );
        assert_ne!(laptop.idempotency_scope(), ci.idempotency_scope());
        assert_ne!(a.idempotency_scope(), laptop.idempotency_scope());

        // Same principal, different session: one namespace, so a request id
        // cannot be replayed for a different effect.
        let other_session = QueueActor::new(
            QueuePrincipal::control("acct", "laptop", Uuid::from_u128(9), "/w"),
            QueueProvenance::default(),
        );
        assert_eq!(
            laptop.idempotency_scope(),
            other_session.idempotency_scope()
        );
    }

    #[test]
    fn authority_revokes_a_removed_credential_without_touching_others() {
        let provenance = QueueProvenance {
            epoch: 3,
            policy: 1,
        };
        let laptop = QueuePrincipal::control("acct", "laptop", session(), "/w").key();
        let ci = QueuePrincipal::control("acct", "ci", session(), "/w").key();

        let both = QueueAuthority::control(
            "acct",
            ["laptop".to_string(), "ci".to_string()],
            ["/w".to_string()],
            provenance,
        )
        .expect("valid authority");
        assert!(both.authorizes(&laptop, session(), "/w"));
        assert!(both.authorizes(&ci, session(), "/w"));

        let rotated = QueueAuthority::control(
            "acct",
            ["laptop".to_string()],
            ["/w".to_string()],
            provenance,
        )
        .expect("valid authority");
        assert!(rotated.authorizes(&laptop, session(), "/w"));
        assert!(
            !rotated.authorizes(&ci, session(), "/w"),
            "removing a credential must revoke its queued execution authority"
        );
    }

    #[test]
    fn dropping_a_workspace_from_the_allowlist_revokes_everyone() {
        let provenance = QueueProvenance::default();
        let laptop = QueuePrincipal::control("acct", "laptop", session(), "/w").key();
        let desktop = QueuePrincipal::desktop(session(), "/w").key();
        let authority = QueueAuthority::control(
            "acct",
            ["laptop".to_string()],
            ["/elsewhere".to_string()],
            provenance,
        )
        .expect("valid authority");
        assert!(!authority.authorizes(&laptop, session(), "/w"));
        assert!(
            !authority.authorizes(&desktop, session(), "/w"),
            "the desktop is not exempt from an allowlist that no longer covers the workspace"
        );
    }

    #[test]
    fn desktop_only_default_authorizes_the_local_ui_and_nobody_else() {
        let authority = QueueAuthority::default();
        let desktop = QueuePrincipal::desktop(session(), "/w").key();
        let control = QueuePrincipal::control("acct", "laptop", session(), "/w").key();
        assert!(authority.authorizes(&desktop, session(), "/w"));
        assert!(
            !authority.authorizes(&control, session(), "/w"),
            "an unconfigured host must not authorize control-plane principals"
        );
    }

    #[test]
    fn a_control_principal_named_desktop_cannot_borrow_local_authority() {
        // The desktop candidate is built under LOCAL_TENANT, so a control
        // credential literally named "desktop" hashes differently and is only
        // authorized if it is in the live principal list.
        let impostor = QueuePrincipal::control("acct", DESKTOP_PRINCIPAL, session(), "/w").key();
        let desktop = QueuePrincipal::desktop(session(), "/w").key();
        assert_ne!(impostor, desktop);
        let authority =
            QueueAuthority::control("acct", [], ["/w".to_string()], QueueProvenance::default())
                .expect("valid test authority");
        assert!(authority.authorizes(&desktop, session(), "/w"));
        assert!(!authority.authorizes(&impostor, session(), "/w"));
    }

    #[test]
    fn delivery_gate_withholds_quarantined_and_revoked_entries() {
        let authority = QueueAuthority::control(
            "acct",
            ["laptop".to_string()],
            ["/w".to_string()],
            QueueProvenance::default(),
        )
        .expect("valid authority");
        let gate = DeliveryGate::new(&authority, session(), "/w");

        let live = QueuePrincipal::control("acct", "laptop", session(), "/w").key();
        let revoked = QueuePrincipal::control("acct", "ci", session(), "/w").key();

        assert!(gate.allows_owner(Some(live.as_str())));
        assert!(
            !gate.allows_owner(Some(revoked.as_str())),
            "a removed credential's queued work must not be delivered"
        );
        assert!(
            !gate.allows_owner(None),
            "legacy principal-less entries must never be delivered"
        );
        assert!(
            !gate.allows_owner(Some("v1-sha256:deadbeef")),
            "an unrecognised ownership handle must fail closed"
        );
    }

    #[test]
    fn an_oversized_principal_list_fails_closed_instead_of_truncating() {
        let many: Vec<String> = (0..(MAX_AUTHORIZED_PRINCIPALS + 64))
            .map(|i| format!("p{i}"))
            .collect();
        let error =
            QueueAuthority::control("acct", many, ["/w".to_string()], QueueProvenance::default())
                .expect_err("an oversized principal list must be refused, not truncated");
        assert!(matches!(
            error,
            QueueAuthorityError::TooManyPrincipals { .. }
        ));
    }

    #[test]
    fn a_quiesced_authority_authorizes_nobody() {
        let authority = QueueAuthority::quiesced();
        let desktop = QueuePrincipal::desktop(session(), "/w").key();
        let control = QueuePrincipal::control("acct", "laptop", session(), "/w").key();
        assert!(!authority.authorizes(&desktop, session(), "/w"));
        assert!(!authority.authorizes(&control, session(), "/w"));
        let gate = DeliveryGate::new(&authority, session(), "/w");
        assert!(!gate.allows_owner(Some(desktop.as_str())));
        assert!(!gate.allows_owner(Some(control.as_str())));
    }
}
