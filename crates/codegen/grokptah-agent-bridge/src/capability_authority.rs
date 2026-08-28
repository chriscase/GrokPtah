//! Host-issued capability generations and one-use effect leases.
//!
//! This module is deliberately not a wire format.  A provider capability
//! record, a model proposal, and a completion claim are all untrusted input
//! until the host binds them to one of these process-owned objects.  The
//! generation is derived from the complete, secret-free snapshot, while the
//! lease is an in-memory object that cannot be deserialized by a caller.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::computer_use::ComputerCapabilities;
use crate::gateway_config::ModelCapabilities;

pub(crate) const CAPABILITY_GENERATION_SCHEMA: &str = "grokptah.capability-generation.v1";
pub(crate) const DEFAULT_CAPABILITY_TTL: Duration = Duration::minutes(15);
const MAX_CAPABILITY_TTL: Duration = Duration::hours(1);
const MAX_EFFECT_LEASE_TTL: Duration = Duration::seconds(30);

/// Opaque principal/auth-generation input supplied by the canonical host seam.
/// Callers cannot construct or deserialize this identity.
#[derive(Clone, PartialEq, Eq)]
pub struct CapabilityPrincipal {
    id: String,
    auth_generation: u64,
}

impl std::fmt::Debug for CapabilityPrincipal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CapabilityPrincipal")
            .field("id", &"[opaque]")
            .field("auth_generation", &"[opaque]")
            .finish()
    }
}

impl CapabilityPrincipal {
    pub(crate) fn new(id: String, auth_generation: u64) -> Result<Self> {
        if id.trim().is_empty() || auth_generation == 0 {
            bail!("canonical capability principal is invalid");
        }
        Ok(Self {
            id,
            auth_generation,
        })
    }

    pub(crate) fn host_default() -> Self {
        Self {
            id: "host-principal".into(),
            auth_generation: 1,
        }
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn auth_generation(&self) -> u64 {
        self.auth_generation
    }
}

/// The effect family to which a host-issued generation is bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CapabilityKind {
    Provider,
    Tool,
    ComputerUse,
}

impl CapabilityKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Tool => "tool",
            Self::ComputerUse => "computer_use",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CapabilityKey {
    kind: CapabilityKind,
    principal: String,
    scope: String,
}

/// A complete capability snapshot assembled by the trusted host.
///
/// It has no `Serialize` or `Deserialize` implementation by design.  The
/// caller can provide a model id or a tool name, but cannot provide the
/// authority object that proves the host accepted the resulting snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilitySnapshot {
    key: CapabilityKey,
    digest: String,
}

impl CapabilitySnapshot {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn provider(
        principal: &str,
        provider_id: &str,
        model_id: &str,
        base_url: &str,
        wire_model_id: &str,
        dialect: &str,
        credential_fingerprint: &str,
        capabilities: &ModelCapabilities,
        policy_digest: &str,
    ) -> Result<Self> {
        let capabilities = serde_json::to_vec(capabilities)?;
        Self::from_parts(
            CapabilityKind::Provider,
            principal,
            model_id,
            [
                provider_id,
                base_url,
                wire_model_id,
                dialect,
                credential_fingerprint,
                policy_digest,
                std::str::from_utf8(&capabilities).unwrap_or_default(),
            ],
        )
    }

    pub(crate) fn computer_use_service(
        principal: &str,
        backend: &ComputerCapabilities,
        policy_digest: &str,
    ) -> Result<Self> {
        let backend = serde_json::to_vec(backend)?;
        let scope = format!("computer-use-service:{policy_digest}");
        Self::from_parts(
            CapabilityKind::ComputerUse,
            principal,
            &scope,
            [
                std::str::from_utf8(&backend).unwrap_or_default(),
                policy_digest,
            ],
        )
    }

    pub(crate) fn tool(principal: &str, tool_name: &str, tool_policy_digest: &str) -> Result<Self> {
        Self::from_parts(
            CapabilityKind::Tool,
            principal,
            tool_name,
            [tool_name, tool_policy_digest],
        )
    }

    fn from_parts<'a>(
        kind: CapabilityKind,
        principal: &str,
        scope: &str,
        parts: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self> {
        if principal.trim().is_empty()
            || scope.trim().is_empty()
            || principal.len() > 256
            || scope.len() > 256
            || principal.contains('\0')
            || scope.contains('\0')
        {
            bail!("malformed capability scope");
        }
        let key = CapabilityKey {
            kind,
            principal: principal.to_string(),
            scope: scope.to_string(),
        };
        let mut digest = Sha256::new();
        digest.update(CAPABILITY_GENERATION_SCHEMA.as_bytes());
        for part in std::iter::once(kind.as_str()).chain(parts) {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
        Ok(Self {
            key,
            digest: format!("{:x}", digest.finalize()),
        })
    }
}

#[derive(Debug, Clone)]
struct CurrentGeneration {
    generation: u64,
    digest: String,
    expires_at: DateTime<Utc>,
}

#[derive(Debug)]
struct AuthorityState {
    process_nonce: String,
    auth_generation: u64,
    enabled: bool,
    next_generation: u64,
    next_attempt: u64,
    current: HashMap<CapabilityKey, CurrentGeneration>,
    envelopes: HashMap<String, CanonicalEffectEnvelope>,
    consumed_leases: HashSet<String>,
}

const INITIAL_AUTH_GENERATION: u64 = 1;

/// A host-installed authority envelope. Request fields can select an allowed
/// operation and exact resource, but they cannot mint or widen this envelope.
#[derive(Debug, Clone)]
pub(crate) struct CanonicalEffectEnvelope {
    envelope_id: String,
    capability: HostCapability,
    snapshot: CapabilitySnapshot,
    principal_id: String,
    incarnation: String,
    auth_generation: u64,
    capability_generation: u64,
    policy_generation: String,
    allowed_operations: BTreeSet<String>,
    resource_scope: String,
    expires_at: DateTime<Utc>,
}

/// Process-owned authority registry.
///
/// The type is public only so host handles can share it with desktop adapters.
/// Its constructor, issuance, validation, and lease methods are crate-private;
/// no SDK/MCP caller can mint or deserialize authority.
#[derive(Clone)]
pub struct CapabilityAuthority {
    state: Arc<Mutex<AuthorityState>>,
}

impl std::fmt::Debug for CapabilityAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CapabilityAuthority")
            .field("state", &"[opaque]")
            .finish()
    }
}

impl CapabilityAuthority {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            state: Arc::new(Mutex::new(AuthorityState {
                process_nonce: Uuid::new_v4().to_string(),
                auth_generation: INITIAL_AUTH_GENERATION,
                enabled,
                next_generation: 0,
                next_attempt: 0,
                current: HashMap::new(),
                envelopes: HashMap::new(),
                consumed_leases: HashSet::new(),
            })),
        }
    }

    pub(crate) fn issue(
        &self,
        snapshot: &CapabilitySnapshot,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> Result<HostCapability> {
        let ttl = ttl.max(Duration::zero()).min(MAX_CAPABILITY_TTL);
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capability authority is unavailable"))?;
        if !state.enabled {
            bail!("capability authority is unavailable");
        }
        let expires_at = now + ttl;
        let current = match state.current.get(&snapshot.key) {
            Some(current) if current.digest == snapshot.digest && current.expires_at > now => {
                current.clone()
            }
            _ => {
                state.next_generation = state.next_generation.saturating_add(1);
                let current = CurrentGeneration {
                    generation: state.next_generation,
                    digest: snapshot.digest.clone(),
                    expires_at,
                };
                state.current.insert(snapshot.key.clone(), current.clone());
                current
            }
        };
        Ok(HostCapability {
            process_nonce: state.process_nonce.clone(),
            key: snapshot.key.clone(),
            digest: snapshot.digest.clone(),
            generation: current.generation,
            expires_at: current.expires_at,
        })
    }

    pub(crate) fn revalidate(
        &self,
        capability: &HostCapability,
        snapshot: &CapabilitySnapshot,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capability authority is unavailable"))?;
        let Some(current) = state.current.get(&snapshot.key) else {
            bail!("capability generation is revoked or foreign");
        };
        if !state.enabled
            || capability.process_nonce != state.process_nonce
            || capability.key != snapshot.key
            || capability.digest != snapshot.digest
            || current.generation != capability.generation
            || current.digest != capability.digest
            || now >= capability.expires_at
            || now >= current.expires_at
        {
            bail!("capability generation is stale, expired, or foreign");
        }
        Ok(())
    }

    pub(crate) fn lease(
        &self,
        capability: &HostCapability,
        snapshot: &CapabilitySnapshot,
        effect_scope: &str,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> Result<EffectLease> {
        self.revalidate(capability, snapshot, now)?;
        if effect_scope.trim().is_empty() || effect_scope.contains('\0') {
            bail!("malformed capability effect scope");
        }
        let expires_at =
            (now + ttl.max(Duration::zero()).min(MAX_EFFECT_LEASE_TTL)).min(capability.expires_at);
        if expires_at <= now {
            bail!("capability effect lease is expired");
        }
        Ok(EffectLease {
            lease_id: Uuid::new_v4().to_string(),
            envelope_id: None,
            attempt_id: None,
            effect_id: None,
            operation: None,
            resource: None,
            process_nonce: capability.process_nonce.clone(),
            key: capability.key.clone(),
            digest: capability.digest.clone(),
            generation: capability.generation,
            effect_scope: effect_scope.to_string(),
            expires_at,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn install_envelope(
        &self,
        envelope_id: &str,
        capability: HostCapability,
        snapshot: CapabilitySnapshot,
        principal_id: &str,
        auth_generation: u64,
        policy_generation: &str,
        allowed_operations: impl IntoIterator<Item = impl Into<String>>,
        resource_scope: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capability authority is unavailable"))?;
        if !state.enabled
            || envelope_id.trim().is_empty()
            || envelope_id.len() > 256
            || resource_scope.trim().is_empty()
            || resource_scope.len() > 256
            || policy_generation.trim().is_empty()
            || snapshot.key.principal != principal_id
            || auth_generation != state.auth_generation
        {
            bail!("canonical capability envelope is invalid or foreign");
        }
        validate_capability_locked(&state, &capability, &snapshot, now)?;
        let allowed_operations = allowed_operations
            .into_iter()
            .map(Into::into)
            .collect::<BTreeSet<_>>();
        if allowed_operations.is_empty()
            || allowed_operations
                .iter()
                .any(|operation| operation.is_empty() || operation.len() > 64)
        {
            bail!("canonical capability envelope has an invalid operation scope");
        }
        let expires_at = (now + DEFAULT_CAPABILITY_TTL).min(capability.expires_at);
        if expires_at <= now {
            bail!("canonical capability envelope is expired");
        }
        let incarnation = state.process_nonce.clone();
        state.envelopes.insert(
            envelope_id.to_string(),
            CanonicalEffectEnvelope {
                envelope_id: envelope_id.to_string(),
                capability_generation: capability.generation,
                capability,
                snapshot,
                principal_id: principal_id.to_string(),
                incarnation,
                auth_generation,
                policy_generation: policy_generation.to_string(),
                allowed_operations,
                resource_scope: resource_scope.to_string(),
                expires_at,
            },
        );
        Ok(())
    }

    /// Lease an effect from an already-installed envelope. This method never
    /// calls `issue`; arbitrary request data is only checked against the
    /// envelope's closed operation/resource scope.
    pub(crate) fn lease_from_envelope(
        &self,
        envelope_id: &str,
        operation: &str,
        resource: &str,
        effect_scope: &str,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> Result<EffectLease> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capability authority is unavailable"))?;
        let envelope = state
            .envelopes
            .get(envelope_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("canonical capability envelope is unavailable"))?;
        validate_envelope_locked(&state, &envelope, now)?;
        if !envelope.allowed_operations.contains(operation)
            || resource != envelope.resource_scope
            || effect_scope.trim().is_empty()
            || effect_scope.len() > 64
        {
            bail!("effect is outside the canonical capability envelope");
        }
        state.next_attempt = state.next_attempt.saturating_add(1);
        let attempt_id = format!("attempt-{}", state.next_attempt);
        let effect_id = Uuid::new_v4().to_string();
        let expires_at =
            (now + ttl.max(Duration::zero()).min(MAX_EFFECT_LEASE_TTL)).min(envelope.expires_at);
        if expires_at <= now {
            bail!("capability effect lease is expired");
        }
        Ok(EffectLease {
            lease_id: Uuid::new_v4().to_string(),
            envelope_id: Some(envelope.envelope_id),
            attempt_id: Some(attempt_id),
            effect_id: Some(effect_id),
            operation: Some(operation.to_string()),
            resource: Some(resource.to_string()),
            process_nonce: envelope.capability.process_nonce,
            key: envelope.capability.key,
            digest: envelope.capability.digest,
            generation: envelope.capability.generation,
            effect_scope: effect_scope.to_string(),
            expires_at,
        })
    }

    pub(crate) fn revalidate_envelope(
        &self,
        envelope_id: &str,
        capability: &HostCapability,
        snapshot: &CapabilitySnapshot,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capability authority is unavailable"))?;
        let envelope = state
            .envelopes
            .get(envelope_id)
            .ok_or_else(|| anyhow::anyhow!("canonical capability envelope is unavailable"))?;
        if envelope.capability.generation != capability.generation
            || envelope.capability.digest != capability.digest
        {
            bail!("canonical capability envelope generation is stale");
        }
        if snapshot != &envelope.snapshot {
            bail!("canonical capability envelope snapshot is stale");
        }
        validate_envelope_locked(&state, envelope, now)
    }

    /// Consume exactly one bounded lease immediately before the effect.
    pub(crate) fn consume(
        &self,
        lease: EffectLease,
        snapshot: &CapabilitySnapshot,
        now: DateTime<Utc>,
    ) -> Result<ConsumedEffect> {
        let capability = HostCapability {
            process_nonce: lease.process_nonce.clone(),
            key: lease.key.clone(),
            digest: lease.digest.clone(),
            generation: lease.generation,
            expires_at: lease.expires_at,
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capability authority is unavailable"))?;
        if let Some(envelope_id) = lease.envelope_id.as_deref() {
            let envelope = state
                .envelopes
                .get(envelope_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("canonical capability envelope is revoked"))?;
            validate_envelope_locked(&state, &envelope, now)?;
            let operation_matches = lease
                .operation
                .as_ref()
                .is_some_and(|operation| envelope.allowed_operations.contains(operation));
            if !operation_matches
                || lease.resource.as_deref() != Some(envelope.resource_scope.as_str())
                || lease.attempt_id.is_none()
                || lease.effect_id.is_none()
            {
                bail!("effect lease is outside its canonical envelope");
            }
        }
        validate_capability_locked(&state, &capability, snapshot, now)?;
        if now >= lease.expires_at {
            bail!("capability effect lease is expired");
        }
        if !state.consumed_leases.insert(lease.lease_id.clone()) {
            bail!("capability effect lease was already consumed");
        }
        Ok(ConsumedEffect {
            lease_id: lease.lease_id,
            effect_scope: lease.effect_scope,
            generation: lease.generation,
            attempt_id: lease.attempt_id,
            effect_id: lease.effect_id,
        })
    }

    pub(crate) fn remove_envelope(&self, envelope_id: &str) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capability authority is unavailable"))?;
        state.envelopes.remove(envelope_id);
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn revoke(&self, snapshot: &CapabilitySnapshot) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capability authority is unavailable"))?;
        state.current.remove(&snapshot.key);
        Ok(())
    }

    pub(crate) fn revoke_all(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capability authority is unavailable"))?;
        state.current.clear();
        state.envelopes.clear();
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn rotate_auth_generation(&self, auth_generation: u64) -> Result<()> {
        if auth_generation == 0 {
            bail!("auth generation must be positive");
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capability authority is unavailable"))?;
        state.auth_generation = auth_generation;
        state.current.clear();
        state.envelopes.clear();
        Ok(())
    }

    #[cfg(test)]
    fn disable_for_test(&self) {
        self.state.lock().unwrap().enabled = false;
    }

    #[cfg(test)]
    pub(crate) fn generation_count_for_test(&self) -> u64 {
        self.state.lock().unwrap().next_generation
    }
}

/// Opaque host-issued capability. It is intentionally not serializable.
#[derive(Debug, Clone)]
pub(crate) struct HostCapability {
    process_nonce: String,
    key: CapabilityKey,
    digest: String,
    generation: u64,
    expires_at: DateTime<Utc>,
}

/// Short-lived, one-use authority consumed at the effect boundary.
#[derive(Debug)]
pub(crate) struct EffectLease {
    lease_id: String,
    envelope_id: Option<String>,
    attempt_id: Option<String>,
    effect_id: Option<String>,
    operation: Option<String>,
    resource: Option<String>,
    process_nonce: String,
    key: CapabilityKey,
    digest: String,
    generation: u64,
    effect_scope: String,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConsumedEffect {
    pub(crate) lease_id: String,
    pub(crate) effect_scope: String,
    pub(crate) generation: u64,
    pub(crate) attempt_id: Option<String>,
    pub(crate) effect_id: Option<String>,
}

fn validate_capability_locked(
    state: &AuthorityState,
    capability: &HostCapability,
    snapshot: &CapabilitySnapshot,
    now: DateTime<Utc>,
) -> Result<()> {
    let Some(current) = state.current.get(&snapshot.key) else {
        bail!("capability generation is revoked or foreign");
    };
    if !state.enabled
        || capability.process_nonce != state.process_nonce
        || capability.key != snapshot.key
        || capability.digest != snapshot.digest
        || current.generation != capability.generation
        || current.digest != capability.digest
        || now >= capability.expires_at
        || now >= current.expires_at
    {
        bail!("capability generation is stale, expired, or foreign");
    }
    Ok(())
}

fn validate_envelope_locked(
    state: &AuthorityState,
    envelope: &CanonicalEffectEnvelope,
    now: DateTime<Utc>,
) -> Result<()> {
    if envelope.principal_id != envelope.snapshot.key.principal
        || envelope.incarnation != state.process_nonce
        || envelope.auth_generation != state.auth_generation
        || envelope.capability_generation != envelope.capability.generation
        || envelope.policy_generation.trim().is_empty()
        || now >= envelope.expires_at
    {
        bail!("canonical capability envelope is stale, expired, or foreign");
    }
    validate_capability_locked(state, &envelope.capability, &envelope.snapshot, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(value: &str) -> CapabilitySnapshot {
        CapabilitySnapshot::from_parts(CapabilityKind::ComputerUse, "agent-owner", "run-1", [value])
            .unwrap()
    }

    #[test]
    fn one_generation_is_stable_until_the_snapshot_changes() {
        let authority = CapabilityAuthority::new(true);
        let now = Utc::now();
        let first = authority
            .issue(&snapshot("same"), now, DEFAULT_CAPABILITY_TTL)
            .unwrap();
        let same = authority
            .issue(
                &snapshot("same"),
                now + Duration::seconds(1),
                DEFAULT_CAPABILITY_TTL,
            )
            .unwrap();
        assert_eq!(first.generation, same.generation);

        let changed = snapshot("downgraded");
        assert!(authority.revalidate(&first, &changed, now).is_err());
        let replacement = authority
            .issue(&changed, now + Duration::seconds(1), DEFAULT_CAPABILITY_TTL)
            .unwrap();
        assert!(replacement.generation > first.generation);
    }

    #[test]
    fn leases_are_bounded_and_one_use() {
        let authority = CapabilityAuthority::new(true);
        let now = Utc::now();
        let snapshot = snapshot("stable");
        let capability = authority
            .issue(&snapshot, now, DEFAULT_CAPABILITY_TTL)
            .unwrap();
        let lease = authority
            .lease(
                &capability,
                &snapshot,
                "computer.act",
                now,
                Duration::minutes(5),
            )
            .unwrap();
        let consumed = authority.consume(lease, &snapshot, now).unwrap();
        assert_eq!(consumed.effect_scope, "computer.act");
    }

    #[test]
    fn revocation_between_admission_and_effect_denies_without_consuming() {
        let authority = CapabilityAuthority::new(true);
        let now = Utc::now();
        let snapshot = snapshot("stable");
        let capability = authority
            .issue(&snapshot, now, DEFAULT_CAPABILITY_TTL)
            .unwrap();
        let lease = authority
            .lease(
                &capability,
                &snapshot,
                "provider.send",
                now,
                Duration::seconds(5),
            )
            .unwrap();
        authority.revoke(&snapshot).unwrap();
        assert!(authority.consume(lease, &snapshot, now).is_err());
    }

    #[test]
    fn installed_envelope_rejects_request_scopes_without_minting() {
        let authority = CapabilityAuthority::new(true);
        let now = Utc::now();
        let snapshot = CapabilitySnapshot::computer_use_service(
            SERVICE_PRINCIPAL_ID,
            &ComputerCapabilities {
                backend_id: "test".into(),
                observe: true,
                semantic_actions: true,
                text_entry: true,
                key_chords: false,
                pointer_fallback: false,
            },
            "policy-v1",
        )
        .unwrap();
        let capability = authority
            .issue(&snapshot, now, DEFAULT_CAPABILITY_TTL)
            .unwrap();
        authority
            .install_envelope(
                "envelope",
                capability,
                snapshot.clone(),
                SERVICE_PRINCIPAL_ID,
                INITIAL_AUTH_GENERATION,
                "policy-v1",
                ["allowed"],
                "resource-1",
                now,
            )
            .unwrap();
        let generations_before = authority.state.lock().unwrap().next_generation;
        assert!(authority
            .lease_from_envelope(
                "envelope",
                "arbitrary",
                "resource-1",
                "durable.arbitrary",
                now,
                Duration::seconds(5),
            )
            .is_err());
        assert_eq!(
            authority.state.lock().unwrap().next_generation,
            generations_before
        );
        let lease = authority
            .lease_from_envelope(
                "envelope",
                "allowed",
                "resource-1",
                "durable.allowed",
                now,
                Duration::seconds(5),
            )
            .unwrap();
        assert!(authority.consume(lease, &snapshot, now).is_ok());
    }

    #[test]
    fn auth_generation_rotation_invalidates_installed_envelopes() {
        let authority = CapabilityAuthority::new(true);
        let now = Utc::now();
        let snapshot = snapshot("rotation");
        let capability = authority
            .issue(&snapshot, now, DEFAULT_CAPABILITY_TTL)
            .unwrap();
        authority
            .install_envelope(
                "rotation-envelope",
                capability,
                snapshot,
                SERVICE_PRINCIPAL_ID,
                INITIAL_AUTH_GENERATION,
                "policy-v1",
                ["act"],
                "resource-1",
                now,
            )
            .unwrap();
        authority.rotate_auth_generation(2).unwrap();
        assert!(authority
            .lease_from_envelope(
                "rotation-envelope",
                "act",
                "resource-1",
                "computer.input",
                now,
                Duration::seconds(5),
            )
            .is_err());
    }

    #[test]
    fn capability_downgrade_replaces_generation_and_stales_old_envelope() {
        let authority = CapabilityAuthority::new(true);
        let now = Utc::now();
        let original = snapshot("semantic-act");
        let capability = authority
            .issue(&original, now, DEFAULT_CAPABILITY_TTL)
            .unwrap();
        authority
            .install_envelope(
                "downgrade-envelope",
                capability,
                original.clone(),
                SERVICE_PRINCIPAL_ID,
                INITIAL_AUTH_GENERATION,
                "policy-v1",
                ["act"],
                "resource-1",
                now,
            )
            .unwrap();
        let downgraded = snapshot("observe-only");
        let _ = authority
            .issue(&downgraded, now, DEFAULT_CAPABILITY_TTL)
            .unwrap();
        assert!(authority
            .lease_from_envelope(
                "downgrade-envelope",
                "act",
                "resource-1",
                "computer.input",
                now,
                Duration::seconds(5),
            )
            .is_err());
    }

    #[test]
    fn revoked_restart_and_foreign_authority_fail_closed() {
        let authority = CapabilityAuthority::new(true);
        let now = Utc::now();
        let stable = snapshot("stable");
        let capability = authority
            .issue(&stable, now, DEFAULT_CAPABILITY_TTL)
            .unwrap();
        authority.revoke(&stable).unwrap();
        assert!(authority.revalidate(&capability, &stable, now).is_err());

        let restarted = CapabilityAuthority::new(true);
        assert!(restarted.revalidate(&capability, &stable, now).is_err());

        authority.disable_for_test();
        assert!(authority
            .issue(&snapshot("other"), now, DEFAULT_CAPABILITY_TTL)
            .is_err());
    }
}
