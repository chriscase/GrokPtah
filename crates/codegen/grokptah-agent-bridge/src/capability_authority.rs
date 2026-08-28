//! Host-issued capability generations and one-use effect leases.
//!
//! This module is deliberately not a wire format.  A provider capability
//! record, a model proposal, and a completion claim are all untrusted input
//! until the host binds them to one of these process-owned objects.  The
//! generation is derived from the complete, secret-free snapshot, while the
//! lease is an in-memory object that cannot be deserialized by a caller.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::computer_use::{ComputerCapabilities, ComputerTarget};
use crate::gateway_config::ModelCapabilities;

pub(crate) const CAPABILITY_GENERATION_SCHEMA: &str = "grokptah.capability-generation.v1";
pub(crate) const DEFAULT_CAPABILITY_TTL: Duration = Duration::minutes(15);
const MAX_CAPABILITY_TTL: Duration = Duration::hours(1);
const MAX_EFFECT_LEASE_TTL: Duration = Duration::seconds(30);

/// The effect family to which a host-issued generation is bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CapabilityKind {
    Provider,
    Tool,
    ComputerUse,
    DurableMutation,
}

impl CapabilityKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Tool => "tool",
            Self::ComputerUse => "computer_use",
            Self::DurableMutation => "durable_mutation",
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

    pub(crate) fn computer_use(
        principal: &str,
        target: &ComputerTarget,
        backend: &ComputerCapabilities,
        provider_generation_digest: Option<&str>,
        policy_digest: &str,
    ) -> Result<Self> {
        let target = serde_json::to_vec(target)?;
        let backend = serde_json::to_vec(backend)?;
        Self::from_parts(
            CapabilityKind::ComputerUse,
            principal,
            "computer-use",
            [
                std::str::from_utf8(&target).unwrap_or_default(),
                std::str::from_utf8(&backend).unwrap_or_default(),
                provider_generation_digest.unwrap_or("none"),
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

    pub(crate) fn durable_mutation(
        principal: &str,
        mutation_name: &str,
        mutation_digest: &str,
    ) -> Result<Self> {
        Self::from_parts(
            CapabilityKind::DurableMutation,
            principal,
            mutation_name,
            [mutation_name, mutation_digest],
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
    enabled: bool,
    next_generation: u64,
    current: HashMap<CapabilityKey, CurrentGeneration>,
    consumed_leases: HashSet<String>,
}

/// Process-owned authority registry.
///
/// The type is public only so host handles can share it with desktop adapters.
/// Its constructor, issuance, validation, and lease methods are crate-private;
/// no SDK/MCP caller can mint or deserialize authority.
#[derive(Debug, Clone)]
pub struct CapabilityAuthority {
    state: Arc<Mutex<AuthorityState>>,
}

impl CapabilityAuthority {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            state: Arc::new(Mutex::new(AuthorityState {
                process_nonce: Uuid::new_v4().to_string(),
                enabled,
                next_generation: 0,
                current: HashMap::new(),
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
            process_nonce: capability.process_nonce.clone(),
            key: capability.key.clone(),
            digest: capability.digest.clone(),
            generation: capability.generation,
            effect_scope: effect_scope.to_string(),
            expires_at,
        })
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
        self.revalidate(&capability, snapshot, now)?;
        if now >= lease.expires_at {
            bail!("capability effect lease is expired");
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capability authority is unavailable"))?;
        if !state.consumed_leases.insert(lease.lease_id.clone()) {
            bail!("capability effect lease was already consumed");
        }
        Ok(ConsumedEffect {
            lease_id: lease.lease_id,
            effect_scope: lease.effect_scope,
            generation: lease.generation,
        })
    }

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
        Ok(())
    }

    #[cfg(test)]
    fn disable_for_test(&self) {
        self.state.lock().unwrap().enabled = false;
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
