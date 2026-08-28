//! Cleanup receipts, recomputed from independent observation.
//!
//! # Why this module looks like this
//!
//! A cleanup receipt asserts that a helper process is gone, a guest is
//! destroyed, an overlay file is deleted, a channel secret is revoked, and a
//! durable occupancy lease is released. If those booleans are supplied by the
//! same code path that performed the teardown, the receipt records an
//! *intention*, not an outcome: a failed `remove_file` whose error was
//! discarded still yields "overlay removed".
//!
//! So the receipt is built by asking a [`CleanupProbe`] to re-derive each fact
//! from its own source — the process table, the VM handle, the filesystem, the
//! channel registry, the occupancy store — after teardown ran. Every probe
//! result is digested individually and the whole set is bound by a single
//! receipt digest. Anything a probe could not determine, or determined to be
//! still present, lands in [`CleanupReceipt::unresolved`] and the guest is not
//! marked clean.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{IsolatedError, IsolatedResult};
use crate::ids::{sha256_hex, validate_digest, validate_id, SCHEMA_VERSION};
use crate::manifest::ComputerSurfaceBinding;

/// One independently re-derived fact about a resource after teardown.
///
/// `Unknown` is deliberately distinct from `Present`: "we could not look" and
/// "it is still there" are different failures, and neither is "released".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceProbeResult {
    /// Which resource this describes, e.g. `overlay`.
    pub resource: String,
    /// How the fact was established, e.g. `filesystem_symlink_metadata`.
    pub method: String,
    pub state: ResourceState,
    /// Probe-specific detail: a path that still exists, an errno, a pid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Guest and incarnation the probe was run against, so a receipt cannot be
    /// satisfied by observations of a different incarnation.
    pub guest_id: String,
    pub surface_incarnation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceState {
    /// Independently confirmed gone.
    Released,
    /// Independently confirmed still present.
    Present,
    /// Could not be determined. Never counts as released.
    Unknown,
}

impl ResourceProbeResult {
    pub fn validate(&self) -> IsolatedResult<()> {
        validate_id("probe resource", &self.resource)?;
        validate_id("probe method", &self.method)?;
        validate_id("probe guest_id", &self.guest_id)?;
        validate_id("probe surface_incarnation", &self.surface_incarnation)?;
        if self.detail.as_ref().is_some_and(|value| value.len() > 512) {
            return Err(IsolatedError::invalid("probe detail exceeds bounds"));
        }
        Ok(())
    }

    pub fn digest(&self) -> String {
        sha256_hex(&serde_json::to_vec(self).unwrap_or_default())
    }

    fn released(&self) -> bool {
        self.state == ResourceState::Released
    }
}

/// The resources a cleanup receipt must account for. Adding a variant forces
/// every probe and every receipt to account for it.
pub const REQUIRED_RESOURCES: &[&str] = &[
    "helper_process",
    "guest_vm",
    "overlay",
    "channel",
    "occupancy",
    "resident_frames",
];

/// Independent re-derivation of post-teardown state.
///
/// Implementations must not consult the bookkeeping that teardown mutated.
/// Removing a key from an in-memory map is not evidence that the underlying
/// resource is gone.
pub trait CleanupProbe: std::fmt::Debug {
    fn probe_id(&self) -> &'static str;

    /// Re-derive each of [`REQUIRED_RESOURCES`] for this guest incarnation.
    fn probe(
        &self,
        guest_id: &str,
        surface: &ComputerSurfaceBinding,
    ) -> IsolatedResult<Vec<ResourceProbeResult>>;

    /// Bytes still resident for this guest, re-read rather than assumed zero.
    fn resident_bytes(&self, guest_id: &str) -> IsolatedResult<u64>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupOutcome {
    /// Every required resource independently confirmed released.
    Exact,
    /// At least one resource is still present or could not be determined.
    Unresolved,
}

/// A receipt over independently observed post-teardown state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CleanupReceipt {
    pub schema_version: u32,
    pub probe_id: String,
    pub guest_id: String,
    pub surface: ComputerSurfaceBinding,
    pub outcome: CleanupOutcome,
    pub resident_bytes: u64,
    pub probes: Vec<ResourceProbeResult>,
    /// Per-resource digests, in `REQUIRED_RESOURCES` order.
    pub probe_digests: Vec<String>,
    /// Digest binding every probe digest, the guest, and the incarnation.
    pub receipt_digest: String,
    /// Human-readable account of what is not resolved. Empty iff `Exact`.
    pub unresolved: Vec<String>,
    pub verified_at: DateTime<Utc>,
}

impl CleanupReceipt {
    /// Build a receipt by asking `probe` to re-derive state. Call only after
    /// teardown has run; the receipt describes what is true afterwards.
    pub fn observe(
        probe: &dyn CleanupProbe,
        guest_id: &str,
        surface: &ComputerSurfaceBinding,
        now: DateTime<Utc>,
    ) -> IsolatedResult<Self> {
        validate_id("guest_id", guest_id)?;
        surface.validate()?;
        let mut unresolved = Vec::new();

        let probes = match probe.probe(guest_id, surface) {
            Ok(results) => results,
            Err(error) => {
                // A probe that cannot run leaves cleanup unresolved rather
                // than defaulting any resource to released.
                return Ok(Self::unresolved_for(
                    probe.probe_id(),
                    guest_id,
                    surface,
                    vec![format!("cleanup probe failed: {}", error.message)],
                    now,
                ));
            }
        };
        for result in &probes {
            result.validate()?;
            if result.guest_id != guest_id || result.surface_incarnation != surface.incarnation {
                return Err(IsolatedError::unauthorized(
                    "cleanup probe result is not bound to the guest surface incarnation",
                ));
            }
        }

        let resident_bytes = match probe.resident_bytes(guest_id) {
            Ok(bytes) => bytes,
            Err(error) => {
                unresolved.push(format!(
                    "resident frame bytes unreadable: {}",
                    error.message
                ));
                u64::MAX
            }
        };

        // Every required resource must be present in the probe set exactly
        // once, so a probe cannot pass by simply omitting a resource.
        let mut ordered = Vec::with_capacity(REQUIRED_RESOURCES.len());
        for required in REQUIRED_RESOURCES {
            let matching: Vec<&ResourceProbeResult> = probes
                .iter()
                .filter(|result| result.resource == *required)
                .collect();
            match matching.as_slice() {
                [] => {
                    unresolved.push(format!("{required}: not observed"));
                }
                [single] => {
                    if !single.released() {
                        unresolved.push(match single.state {
                            ResourceState::Present => format!(
                                "{required}: still present{}",
                                single
                                    .detail
                                    .as_deref()
                                    .map(|d| format!(" ({d})"))
                                    .unwrap_or_default()
                            ),
                            _ => format!(
                                "{required}: state unknown{}",
                                single
                                    .detail
                                    .as_deref()
                                    .map(|d| format!(" ({d})"))
                                    .unwrap_or_default()
                            ),
                        });
                    }
                    ordered.push((*single).clone());
                }
                _ => {
                    unresolved.push(format!("{required}: contradictory observations"));
                }
            }
        }
        if resident_bytes != 0 && resident_bytes != u64::MAX {
            unresolved.push(format!(
                "resident_frames: {resident_bytes} bytes still held"
            ));
        }

        let probe_digests: Vec<String> = ordered.iter().map(ResourceProbeResult::digest).collect();
        let outcome = if unresolved.is_empty() && ordered.len() == REQUIRED_RESOURCES.len() {
            CleanupOutcome::Exact
        } else {
            CleanupOutcome::Unresolved
        };
        let receipt = Self {
            schema_version: SCHEMA_VERSION,
            probe_id: probe.probe_id().to_string(),
            guest_id: guest_id.to_string(),
            surface: surface.clone(),
            outcome,
            resident_bytes: if resident_bytes == u64::MAX {
                0
            } else {
                resident_bytes
            },
            receipt_digest: receipt_digest(guest_id, surface, &probe_digests, outcome),
            probes: ordered,
            probe_digests,
            unresolved,
            verified_at: now,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    fn unresolved_for(
        probe_id: &str,
        guest_id: &str,
        surface: &ComputerSurfaceBinding,
        unresolved: Vec<String>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            probe_id: probe_id.to_string(),
            guest_id: guest_id.to_string(),
            surface: surface.clone(),
            outcome: CleanupOutcome::Unresolved,
            resident_bytes: 0,
            probes: Vec::new(),
            probe_digests: Vec::new(),
            receipt_digest: receipt_digest(guest_id, surface, &[], CleanupOutcome::Unresolved),
            unresolved,
            verified_at: now,
        }
    }

    /// Structural validation. A receipt whose digests do not recompute, or
    /// whose outcome disagrees with its own probe set, is rejected — so a
    /// hand-written `Exact` receipt cannot be passed off as observed.
    pub fn validate(&self) -> IsolatedResult<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(IsolatedError::internal(
                "cleanup receipt schema is unsupported",
            ));
        }
        validate_id("guest_id", &self.guest_id)?;
        validate_id("probe_id", &self.probe_id)?;
        self.surface.validate()?;
        for digest in &self.probe_digests {
            validate_digest("probe digest", digest)?;
        }
        validate_digest("receipt digest", &self.receipt_digest)?;
        if self.probes.len() != self.probe_digests.len() {
            return Err(IsolatedError::internal(
                "cleanup receipt digest count does not match its probe set",
            ));
        }
        for (result, digest) in self.probes.iter().zip(&self.probe_digests) {
            result.validate()?;
            if &result.digest() != digest {
                return Err(IsolatedError::unauthorized(
                    "cleanup receipt digest does not recompute from its probe result",
                ));
            }
            if result.guest_id != self.guest_id
                || result.surface_incarnation != self.surface.incarnation
            {
                return Err(IsolatedError::unauthorized(
                    "cleanup receipt binds a probe from another guest incarnation",
                ));
            }
        }
        if self.receipt_digest
            != receipt_digest(
                &self.guest_id,
                &self.surface,
                &self.probe_digests,
                self.outcome,
            )
        {
            return Err(IsolatedError::unauthorized(
                "cleanup receipt digest does not bind its own contents",
            ));
        }
        let all_released = self.probes.iter().all(ResourceProbeResult::released)
            && self.probes.len() == REQUIRED_RESOURCES.len()
            && self.resident_bytes == 0;
        match self.outcome {
            CleanupOutcome::Exact if !all_released || !self.unresolved.is_empty() => {
                Err(IsolatedError::unauthorized(
                    "cleanup receipt claims Exact but its own probes do not support it",
                ))
            }
            CleanupOutcome::Unresolved if self.unresolved.is_empty() => Err(
                IsolatedError::internal("unresolved cleanup receipt lists no reason"),
            ),
            _ => Ok(()),
        }
    }

    pub fn is_exact(&self) -> bool {
        self.outcome == CleanupOutcome::Exact
    }

    /// Turn an unresolved receipt into the uncertain error callers must
    /// surface rather than swallow.
    pub fn require_exact(&self) -> IsolatedResult<()> {
        self.validate()?;
        if self.is_exact() {
            return Ok(());
        }
        Err(IsolatedError::uncertain(format!(
            "isolated guest cleanup is unresolved: {}",
            self.unresolved.join("; ")
        )))
    }
}

fn receipt_digest(
    guest_id: &str,
    surface: &ComputerSurfaceBinding,
    probe_digests: &[String],
    outcome: CleanupOutcome,
) -> String {
    let mut payload = format!(
        "grokptah-cleanup-receipt-v1\0{guest_id}\0{}\0{}\0{}\0",
        surface.surface_id,
        surface.incarnation,
        match outcome {
            CleanupOutcome::Exact => "exact",
            CleanupOutcome::Unresolved => "unresolved",
        }
    );
    for digest in probe_digests {
        payload.push_str(digest);
        payload.push('\0');
    }
    sha256_hex(payload.as_bytes())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedCleanupReason {
    Success,
    Cancel,
    Timeout,
    HelperFailure,
    GuestCrash,
    HostCrash,
    Disconnect,
    Restart,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct ScriptedProbe {
        results: Vec<ResourceProbeResult>,
        resident: IsolatedResult<u64>,
        fail: bool,
    }

    impl CleanupProbe for ScriptedProbe {
        fn probe_id(&self) -> &'static str {
            "scripted_probe_v1"
        }
        fn probe(
            &self,
            _guest_id: &str,
            _surface: &ComputerSurfaceBinding,
        ) -> IsolatedResult<Vec<ResourceProbeResult>> {
            if self.fail {
                return Err(IsolatedError::internal("probe exploded"));
            }
            Ok(self.results.clone())
        }
        fn resident_bytes(&self, _guest_id: &str) -> IsolatedResult<u64> {
            self.resident.clone()
        }
    }

    fn surface() -> ComputerSurfaceBinding {
        ComputerSurfaceBinding {
            surface_id: "surface-1".into(),
            incarnation: "incarnation-1".into(),
        }
    }

    fn all_released() -> Vec<ResourceProbeResult> {
        REQUIRED_RESOURCES
            .iter()
            .map(|resource| ResourceProbeResult {
                resource: (*resource).into(),
                method: "unit_fixture".into(),
                state: ResourceState::Released,
                detail: None,
                guest_id: "guest-1".into(),
                surface_incarnation: "incarnation-1".into(),
            })
            .collect()
    }

    fn observe(probe: ScriptedProbe) -> CleanupReceipt {
        CleanupReceipt::observe(&probe, "guest-1", &surface(), Utc::now()).unwrap()
    }

    #[test]
    fn fully_released_probes_yield_an_exact_receipt() {
        let receipt = observe(ScriptedProbe {
            results: all_released(),
            resident: Ok(0),
            fail: false,
        });
        assert!(receipt.is_exact());
        receipt.require_exact().unwrap();
        assert_eq!(receipt.probe_digests.len(), REQUIRED_RESOURCES.len());
    }

    #[test]
    fn a_resource_still_present_is_surfaced_not_swallowed() {
        let mut results = all_released();
        results[2].state = ResourceState::Present;
        results[2].detail = Some("overlay file still on disk".into());
        let receipt = observe(ScriptedProbe {
            results,
            resident: Ok(0),
            fail: false,
        });
        assert_eq!(receipt.outcome, CleanupOutcome::Unresolved);
        assert!(receipt.unresolved.iter().any(|r| r.contains("overlay")));
        assert_eq!(
            receipt.require_exact().unwrap_err().code,
            crate::error::IsolatedErrorCode::UncertainOutcome
        );
    }

    #[test]
    fn an_unknown_resource_never_counts_as_released() {
        let mut results = all_released();
        results[0].state = ResourceState::Unknown;
        let receipt = observe(ScriptedProbe {
            results,
            resident: Ok(0),
            fail: false,
        });
        assert_eq!(receipt.outcome, CleanupOutcome::Unresolved);
        assert!(receipt
            .unresolved
            .iter()
            .any(|r| r.contains("helper_process") && r.contains("unknown")));
    }

    #[test]
    fn an_omitted_resource_cannot_pass_by_silence() {
        let mut results = all_released();
        results.retain(|result| result.resource != "occupancy");
        let receipt = observe(ScriptedProbe {
            results,
            resident: Ok(0),
            fail: false,
        });
        assert_eq!(receipt.outcome, CleanupOutcome::Unresolved);
        assert!(receipt
            .unresolved
            .iter()
            .any(|r| r.contains("occupancy: not observed")));
    }

    #[test]
    fn a_probe_that_cannot_run_leaves_cleanup_unresolved() {
        let receipt = observe(ScriptedProbe {
            results: Vec::new(),
            resident: Ok(0),
            fail: true,
        });
        assert_eq!(receipt.outcome, CleanupOutcome::Unresolved);
        assert!(receipt.unresolved[0].contains("probe failed"));
    }

    #[test]
    fn unreadable_resident_bytes_leave_cleanup_unresolved() {
        let receipt = observe(ScriptedProbe {
            results: all_released(),
            resident: Err(IsolatedError::internal("cannot read")),
            fail: false,
        });
        assert_eq!(receipt.outcome, CleanupOutcome::Unresolved);
        assert!(receipt
            .unresolved
            .iter()
            .any(|r| r.contains("resident frame bytes")));
    }

    #[test]
    fn a_fabricated_exact_receipt_does_not_validate() {
        let real = observe(ScriptedProbe {
            results: all_released(),
            resident: Ok(0),
            fail: false,
        });

        // Flip every probe to Present but keep the Exact claim and digests.
        let mut forged = real.clone();
        for probe in &mut forged.probes {
            probe.state = ResourceState::Present;
        }
        assert!(forged.validate().is_err());

        // Claim Exact with no probes at all.
        let mut empty = real.clone();
        empty.probes.clear();
        empty.probe_digests.clear();
        assert!(empty.validate().is_err());

        // Keep the probes but forge the receipt digest.
        let mut rehashed = real.clone();
        rehashed.receipt_digest = sha256_hex(b"whatever");
        assert!(rehashed.validate().is_err());
    }

    #[test]
    fn probes_from_another_incarnation_are_refused() {
        let mut results = all_released();
        results[1].surface_incarnation = "old-incarnation".into();
        let error = CleanupReceipt::observe(
            &ScriptedProbe {
                results,
                resident: Ok(0),
                fail: false,
            },
            "guest-1",
            &surface(),
            Utc::now(),
        )
        .unwrap_err();
        assert_eq!(error.code, crate::error::IsolatedErrorCode::Unauthorized);
    }
}
