//! Who is asking, and under what policy.
//!
//! A snapshot is issued to exactly one principal and pinned to a fingerprint
//! of the authorization inputs that produced it. At action time both are
//! recomputed and compared: a snapshot cannot outlive the authorization it
//! was derived from, and a token cannot be replayed across principals.

use crate::digest::{tagged_digest, to_hex};

/// The acting identity. Every field participates in the fingerprint, so
/// changing any one of them invalidates outstanding tokens.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Principal {
    pub principal_id: String,
    pub tenant_id: String,
    pub project_id: String,
    pub session_id: String,
}

impl Principal {
    pub fn new(
        principal_id: impl Into<String>,
        tenant_id: impl Into<String>,
        project_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            principal_id: principal_id.into(),
            tenant_id: tenant_id.into(),
            project_id: project_id.into(),
            session_id: session_id.into(),
        }
    }

    /// Stable fingerprint over all four fields.
    pub fn fingerprint(&self) -> String {
        to_hex(&tagged_digest(
            "grokptah.source-view.principal.v1",
            &[
                self.principal_id.as_bytes(),
                self.tenant_id.as_bytes(),
                self.project_id.as_bytes(),
                self.session_id.as_bytes(),
            ],
        ))
    }
}

/// The authorization inputs a snapshot was derived from.
///
/// This is deliberately *not* the root list: it is the upstream state that
/// decided the root list. If it moves, the derived roots are stale even when
/// they happen to still describe the same directories.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicyInputs {
    /// Ordered, already-normalised authorization facts. Callers push whatever
    /// their policy actually reads: the project workspace, each run's identity
    /// and promotion state, the permission mode, and so on.
    facts: Vec<String>,
}

impl PolicyInputs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one authorization fact. Order is significant and preserved.
    pub fn push(&mut self, key: &str, value: &str) {
        self.facts.push(format!("{key}={value}"));
    }

    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.facts.len()
    }

    /// Fingerprint over every recorded fact, in order.
    pub fn fingerprint(&self) -> String {
        let fields: Vec<&[u8]> = self.facts.iter().map(|fact| fact.as_bytes()).collect();
        to_hex(&tagged_digest("grokptah.source-view.policy.v1", &fields))
    }
}

/// The pair a snapshot is bound to, recomputed at action time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationContext {
    pub principal: Principal,
    pub policy: PolicyInputs,
}

impl AuthorizationContext {
    pub fn new(principal: Principal, policy: PolicyInputs) -> Self {
        Self { principal, policy }
    }
}
