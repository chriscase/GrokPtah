//! Capability and bounds admission.
//!
//! Two rules hold everywhere in this module.
//!
//! *Default deny.* An operation is refused unless the configured capability
//! set advertises it as `available`, or advertises it as `gated` **and** the
//! operator recorded an explicit grant. Absence is never permission.
//!
//! *Narrow only.* Caller bounds may shrink a host ceiling, never raise one. A
//! request above the ceiling is refused rather than silently clamped, so a
//! caller is never told a larger budget was accepted.
//!
//! The capability identifiers are the ones the desktop authority already
//! advertises; the headless host enforces that vocabulary rather than minting a
//! parallel one.

use std::collections::BTreeSet;

use grokptah_agent_sdk::run::Bounds;
use grokptah_agent_sdk::{CapabilityAvailability, CapabilitySet};

use crate::config::{HostConfig, HostLimits};
use crate::error::{HostError, HostResult};

/// Read bounded, redacted host and run projections.
pub const CAP_OBSERVE: &str = "session.observe";
/// Submit, cancel, and pause bounded runs.
pub const CAP_EXECUTE: &str = "run.execute";
/// Steer a run that is already admitted.
pub const CAP_QUEUE: &str = "run.queue";
/// Read review projections and receipts.
pub const CAP_REVIEW: &str = "run.review";
/// Resume a halted run with an explicit operator action.
pub const CAP_RESUME: &str = "agent.resume";
/// Human-gated approval. Also gates allowing a run past a raised escalation.
pub const CAP_PROMOTE: &str = "run.promote";

/// Host ceilings resolved for one admitted run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedBounds {
    /// Accepted prompt byte ceiling.
    pub max_prompt_bytes: u32,
    /// Accepted model round ceiling.
    pub max_rounds: u16,
    /// Accepted wall-clock ceiling.
    pub max_duration_ms: u64,
}

impl ResolvedBounds {
    /// Project the resolved ceiling into the public bounds contract.
    pub fn to_public(self) -> Bounds {
        Bounds {
            max_prompt_bytes: Some(self.max_prompt_bytes),
            max_rounds: Some(self.max_rounds),
            max_duration_ms: Some(self.max_duration_ms),
        }
    }
}

/// Capability and bounds gate for every host operation.
#[derive(Debug, Clone)]
pub struct Authority {
    capabilities: CapabilitySet,
    grants: BTreeSet<String>,
    limits: HostLimits,
}

impl Authority {
    /// Build the gate from validated configuration.
    pub fn new(config: &HostConfig) -> Self {
        Self {
            capabilities: config.capabilities.clone(),
            grants: config.grants.iter().cloned().collect(),
            limits: config.limits.clone(),
        }
    }

    /// Refuse unless the capability is advertised and, if gated, granted.
    pub fn require(&self, capability_id: &str) -> HostResult<()> {
        let Some(descriptor) = self.capabilities.get(capability_id) else {
            return Err(HostError::forbidden(
                "capability_unknown",
                "the host does not advertise this capability",
            ));
        };
        match descriptor.availability {
            CapabilityAvailability::Unavailable => Err(HostError::forbidden(
                "capability_unavailable",
                "the capability is unavailable on this host",
            )),
            CapabilityAvailability::Gated => {
                if self.grants.contains(capability_id) {
                    Ok(())
                } else {
                    Err(HostError::forbidden(
                        "capability_gated",
                        "the capability requires an explicit operator grant",
                    ))
                }
            }
            CapabilityAvailability::Available => {
                if descriptor.human_gate && !self.grants.contains(capability_id) {
                    return Err(HostError::forbidden(
                        "capability_human_gate",
                        "the capability requires an explicit human gate",
                    ));
                }
                Ok(())
            }
        }
    }

    /// Resolve caller bounds against the host ceilings.
    pub fn admit_bounds(&self, requested: Option<&Bounds>) -> HostResult<ResolvedBounds> {
        let ceiling = ResolvedBounds {
            max_prompt_bytes: self.limits.max_prompt_bytes,
            max_rounds: self.limits.max_rounds,
            max_duration_ms: self.limits.max_duration_ms,
        };
        let Some(requested) = requested else {
            return Ok(ceiling);
        };
        requested.validate().map_err(|reason| {
            HostError::invalid("bounds_invalid", format!("bounds are not valid: {reason}"))
        })?;

        let prompt_bytes = narrow_u32(
            requested.max_prompt_bytes,
            ceiling.max_prompt_bytes,
            "maxPromptBytes",
        )?;
        let rounds = narrow_u16(requested.max_rounds, ceiling.max_rounds, "maxRounds")?;
        let duration = narrow_u64(
            requested.max_duration_ms,
            ceiling.max_duration_ms,
            "maxDurationMs",
        )?;

        Ok(ResolvedBounds {
            max_prompt_bytes: prompt_bytes,
            max_rounds: rounds,
            max_duration_ms: duration,
        })
    }

    /// Capability identifiers the host can honor for this operator.
    pub fn permitted_ids(&self) -> Vec<String> {
        self.capabilities
            .capabilities
            .iter()
            .filter(|descriptor| self.require(&descriptor.id).is_ok())
            .map(|descriptor| descriptor.id.clone())
            .collect()
    }

    /// The advertised capability set, for the `capabilities` projection.
    pub fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }
}

fn exceeded(field: &str) -> HostError {
    HostError::invalid(
        "bounds_exceed_ceiling",
        format!("{field} exceeds the host ceiling"),
    )
}

fn narrow_u32(requested: Option<u32>, ceiling: u32, field: &str) -> HostResult<u32> {
    match requested {
        Some(value) if value > ceiling => Err(exceeded(field)),
        Some(value) => Ok(value),
        None => Ok(ceiling),
    }
}

fn narrow_u16(requested: Option<u16>, ceiling: u16, field: &str) -> HostResult<u16> {
    match requested {
        Some(value) if value > ceiling => Err(exceeded(field)),
        Some(value) => Ok(value),
        None => Ok(ceiling),
    }
}

fn narrow_u64(requested: Option<u64>, ceiling: u64, field: &str) -> HostResult<u64> {
    match requested {
        Some(value) if value > ceiling => Err(exceeded(field)),
        Some(value) => Ok(value),
        None => Ok(ceiling),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn unknown_unavailable_and_ungranted_capabilities_are_all_denied() {
        let config = testing::config_fixture();
        let authority = Authority::new(&config);

        assert!(authority.require(CAP_OBSERVE).is_ok());
        assert_eq!(
            authority
                .require("run.invented")
                .expect_err("unknown is denied")
                .reason_code(),
            "capability_unknown"
        );
        assert_eq!(
            authority
                .require(CAP_RESUME)
                .expect_err("unavailable is denied")
                .reason_code(),
            "capability_unavailable"
        );
        assert_eq!(
            authority
                .require(CAP_PROMOTE)
                .expect_err("gated without a grant is denied")
                .reason_code(),
            "capability_gated"
        );
    }

    #[test]
    fn an_explicit_grant_opens_exactly_one_gated_capability() {
        let mut config = testing::config_fixture();
        config.grants = vec![CAP_PROMOTE.to_owned()];
        config.validate().expect("grant is valid");
        let authority = Authority::new(&config);

        assert!(authority.require(CAP_PROMOTE).is_ok());
        assert!(authority.permitted_ids().contains(&CAP_PROMOTE.to_owned()));
        assert!(!authority.permitted_ids().contains(&CAP_RESUME.to_owned()));
    }

    #[test]
    fn bounds_narrow_but_never_widen() {
        let config = testing::config_fixture();
        let authority = Authority::new(&config);

        let defaulted = authority.admit_bounds(None).expect("ceiling applies");
        assert_eq!(defaulted.max_rounds, config.limits.max_rounds);

        let narrowed = authority
            .admit_bounds(Some(&Bounds {
                max_rounds: Some(2),
                ..Bounds::default()
            }))
            .expect("narrowing is allowed");
        assert_eq!(narrowed.max_rounds, 2);
        assert_eq!(narrowed.max_prompt_bytes, config.limits.max_prompt_bytes);

        let widened = authority.admit_bounds(Some(&Bounds {
            max_rounds: Some(config.limits.max_rounds + 1),
            ..Bounds::default()
        }));
        assert_eq!(
            widened.expect_err("widening is refused").reason_code(),
            "bounds_exceed_ceiling"
        );
    }

    #[test]
    fn contract_invalid_bounds_are_refused_before_admission() {
        let config = testing::config_fixture();
        let authority = Authority::new(&config);
        let error = authority
            .admit_bounds(Some(&Bounds {
                max_rounds: Some(0),
                ..Bounds::default()
            }))
            .expect_err("zero rounds is refused");
        assert_eq!(error.reason_code(), "bounds_invalid");
    }
}
