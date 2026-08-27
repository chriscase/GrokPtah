//! Explicit capability discovery.
//!
//! ADR-002 §3 records that host capabilities are declared and fail closed, and
//! that "explicit capability advertisement is a required future contract"
//! which "must define stable capability identifiers, the host/version that
//! asserted them, attempt-time capture, and typed unsupported/forbidden
//! failures." This module is that contract.
//!
//! Three properties matter more than the field list:
//!
//! * A capability that is **absent** and a capability that is **denied** are
//!   different answers. A desktop host without Computer Use reports
//!   `Unsupported`; a host that has it but will not delegate it over this
//!   boundary reports `Forbidden`. A consumer must not have to guess.
//! * Some identifiers are **permanently forbidden** at this seam regardless of
//!   host. Provider credentials and Computer Use *control* are the two that
//!   exist today, and [`CapabilityDocument::new`] stamps them itself so no
//!   adapter can advertise them by mistake.
//! * The document names the host and version that asserted it, so an attempt
//!   can capture what was true when it started.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{SdkError, SdkErrorCode, SdkResult};
use crate::ids::Label;
use crate::version::{ContractVersion, CONTRACT_VERSION};

/// Stable capability identifier.
///
/// Wire tokens are dotted lowercase and never change meaning within a contract
/// major. Unknown identifiers decode to [`CapabilityId::Unknown`] so a newer
/// host can advertise more without breaking an older consumer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum CapabilityId {
    /// Create a Build session bound to an already-allowlisted workspace.
    SessionCreate,
    /// List the sessions this credential may address.
    SessionList,
    /// Submit one finite task run.
    TaskSubmit,
    /// Read one run's public projection.
    RunObserve,
    /// Page a run's bounded public event journal.
    RunEventsPage,
    /// Follow a run's live event channel.
    RunEventsLive,
    /// Non-cancelling follow-up steering into an active or idle session.
    RunFollowUp,
    /// Explicit run cancellation.
    RunCancel,
    /// Acquire, renew, and release a durable work lease.
    ControlLease,
    /// Fetch a bounded, digest-verified artifact.
    ArtifactFetch,
    /// Read-only Computer Run projections.
    ComputerRead,
    /// Read the redacted receipts of mutations already performed.
    ReceiptRead,

    // ── Permanently forbidden at this seam ────────────────────────────────
    /// Computer Use *mutation*: actions, grants, evidence bytes, screenshots.
    /// The runtime exposes no such tool over its control plane and this seam
    /// must not become the first one.
    ComputerControl,
    /// Any read of, or delegation of, provider credentials or auth material.
    ProviderCredentials,

    /// Forward compatibility.
    Unknown(String),
}

impl CapabilityId {
    pub fn as_wire(&self) -> &str {
        match self {
            Self::SessionCreate => "session.create",
            Self::SessionList => "session.list",
            Self::TaskSubmit => "task.submit",
            Self::RunObserve => "run.observe",
            Self::RunEventsPage => "run.events.page",
            Self::RunEventsLive => "run.events.live",
            Self::RunFollowUp => "run.followup",
            Self::RunCancel => "run.cancel",
            Self::ControlLease => "control.lease",
            Self::ArtifactFetch => "artifact.fetch",
            Self::ComputerRead => "computer.read",
            Self::ReceiptRead => "receipt.read",
            Self::ComputerControl => "computer.control",
            Self::ProviderCredentials => "provider.credentials",
            Self::Unknown(raw) => raw.as_str(),
        }
    }

    pub fn from_wire(raw: &str) -> Self {
        match raw {
            "session.create" => Self::SessionCreate,
            "session.list" => Self::SessionList,
            "task.submit" => Self::TaskSubmit,
            "run.observe" => Self::RunObserve,
            "run.events.page" => Self::RunEventsPage,
            "run.events.live" => Self::RunEventsLive,
            "run.followup" => Self::RunFollowUp,
            "run.cancel" => Self::RunCancel,
            "control.lease" => Self::ControlLease,
            "artifact.fetch" => Self::ArtifactFetch,
            "computer.read" => Self::ComputerRead,
            "receipt.read" => Self::ReceiptRead,
            "computer.control" => Self::ComputerControl,
            "provider.credentials" => Self::ProviderCredentials,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// Whether exercising this capability changes durable state.
    ///
    /// A read-only observer may hold only the read side. An identifier this
    /// build does not recognize counts as a **mutation**: refusing an unknown
    /// capability to an observer is recoverable, granting one is not.
    pub fn is_mutation(&self) -> bool {
        match self {
            Self::SessionList
            | Self::RunObserve
            | Self::RunEventsPage
            | Self::RunEventsLive
            | Self::ArtifactFetch
            | Self::ComputerRead
            | Self::ReceiptRead => false,
            Self::SessionCreate
            | Self::TaskSubmit
            | Self::RunFollowUp
            | Self::RunCancel
            | Self::ControlLease
            | Self::ComputerControl
            | Self::ProviderCredentials => true,
            Self::Unknown(_) => true,
        }
    }

    /// Capabilities this seam refuses to carry on any host, ever.
    ///
    /// Removing an entry here is a contract **major** change and a security
    /// decision, not a feature toggle.
    pub fn permanently_forbidden() -> &'static [CapabilityId] {
        static DENIED: &[CapabilityId] = &[
            CapabilityId::ComputerControl,
            CapabilityId::ProviderCredentials,
        ];
        DENIED
    }

    pub fn is_permanently_forbidden(&self) -> bool {
        Self::permanently_forbidden().contains(self)
    }
}

impl Serialize for CapabilityId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for CapabilityId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(Self::from_wire(&raw))
    }
}

impl std::fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// Which host asserted a capability document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    /// In-process runtime inside the desktop application.
    Desktop,
    /// Headless `grokptah-service`, local VM or private host.
    Service,
    /// A deterministic fake. Never a production host.
    Fake,
}

/// Non-secret host identity captured with every document and every attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostDescriptor {
    pub kind: HostKind,
    /// Product name, e.g. `GrokPtah`.
    pub product: Label,
    /// Host build identity. Opaque to the consumer; useful in a bug report.
    pub host_version: Label,
}

/// Whether one capability can be used right now, and if not, why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Availability {
    /// Usable by this credential on this host.
    Available,
    /// This host cannot provide it at all (missing OS support, no ledger, not
    /// compiled in). Maps to [`SdkErrorCode::Unsupported`].
    Unsupported { reason: Label },
    /// The host could provide it, but this credential or this boundary may not
    /// have it. Maps to [`SdkErrorCode::ForbiddenScope`].
    Forbidden { reason: Label },
}

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    /// The typed failure a caller receives when it uses the capability anyway.
    pub fn denial(&self, id: &CapabilityId) -> Option<SdkError> {
        match self {
            Self::Available => None,
            Self::Unsupported { reason } => Some(
                SdkError::new(
                    SdkErrorCode::Unsupported,
                    format!("capability {id} is unsupported on this host: {reason}"),
                )
                .with_detail("capability", id.as_wire()),
            ),
            Self::Forbidden { reason } => Some(
                SdkError::new(
                    SdkErrorCode::ForbiddenScope,
                    format!("capability {id} is forbidden for this caller: {reason}"),
                )
                .with_detail("capability", id.as_wire()),
            ),
        }
    }
}

/// One advertised capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityDescriptor {
    pub id: CapabilityId,
    /// Contract minor at which this capability was introduced. A consumer
    /// negotiated below this minor must not call it.
    pub since: ContractVersion,
    pub availability: Availability,
}

/// Bounds a consumer must respect. Advertised rather than hard-coded so a
/// stricter host can narrow them without a contract change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryLimits {
    /// Largest event page a host will return.
    pub max_event_page: u32,
    /// Page size used when a caller does not ask for one.
    pub default_event_page: u32,
    /// Largest artifact body the seam will carry inline.
    pub max_artifact_bytes: u64,
    /// Largest follow-up / prompt payload.
    pub max_prompt_bytes: u64,
}

impl Default for BoundaryLimits {
    fn default() -> Self {
        // These mirror the runtime's shipped ceilings: `MAX_EVENT_PAGE` /
        // `DEFAULT_EVENT_PAGE` from the Computer Use projection, the control
        // plane's 1..=500 event page clamp, and `RunBounds::max_prompt_bytes`.
        Self {
            max_event_page: 500,
            default_event_page: 100,
            max_artifact_bytes: 1024 * 1024,
            max_prompt_bytes: 100_000,
        }
    }
}

/// What one host offers one credential at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityDocument {
    pub contract_version: ContractVersion,
    pub host: HostDescriptor,
    pub asserted_at: DateTime<Utc>,
    pub limits: BoundaryLimits,
    capabilities: BTreeMap<String, CapabilityDescriptor>,
}

impl CapabilityDocument {
    /// Build a document, stamping the permanently forbidden identifiers.
    ///
    /// Any caller-supplied entry for a permanently forbidden identifier is
    /// **overwritten**, not merged. An adapter cannot advertise Computer Use
    /// control or provider credentials as available even by accident.
    pub fn new(
        host: HostDescriptor,
        asserted_at: DateTime<Utc>,
        limits: BoundaryLimits,
        offered: impl IntoIterator<Item = CapabilityDescriptor>,
    ) -> Self {
        let mut capabilities: BTreeMap<String, CapabilityDescriptor> = offered
            .into_iter()
            .filter(|d| !d.id.is_permanently_forbidden())
            .map(|d| (d.id.as_wire().to_string(), d))
            .collect();
        for id in CapabilityId::permanently_forbidden() {
            let reason =
                Label::new("this capability is never delegated across the public SDK boundary")
                    .expect("static reason is a valid label");
            capabilities.insert(
                id.as_wire().to_string(),
                CapabilityDescriptor {
                    id: id.clone(),
                    since: ContractVersion::new(CONTRACT_VERSION.major, 0),
                    availability: Availability::Forbidden { reason },
                },
            );
        }
        Self {
            contract_version: CONTRACT_VERSION,
            host,
            asserted_at,
            limits,
            capabilities,
        }
    }

    pub fn get(&self, id: &CapabilityId) -> Option<&CapabilityDescriptor> {
        self.capabilities.get(id.as_wire())
    }

    pub fn iter(&self) -> impl Iterator<Item = &CapabilityDescriptor> {
        self.capabilities.values()
    }

    pub fn is_available(&self, id: &CapabilityId) -> bool {
        self.get(id)
            .map(|d| d.availability.is_available())
            .unwrap_or(false)
    }

    /// Gate a call on a capability.
    ///
    /// An identifier the host never mentioned is
    /// [`SdkErrorCode::CapabilityUnavailable`], which is distinct from a host
    /// that mentioned it and said no. "I do not know about this" and "I refuse
    /// this" are different facts and a consumer may reasonably act on each.
    pub fn require(&self, id: &CapabilityId) -> SdkResult<&CapabilityDescriptor> {
        let Some(descriptor) = self.get(id) else {
            return Err(SdkError::new(
                SdkErrorCode::CapabilityUnavailable,
                format!("host did not advertise capability {id}"),
            )
            .with_detail("capability", id.as_wire()));
        };
        if let Some(denial) = descriptor.availability.denial(id) {
            return Err(denial);
        }
        Ok(descriptor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> HostDescriptor {
        HostDescriptor {
            kind: HostKind::Fake,
            product: Label::new("GrokPtah").unwrap(),
            host_version: Label::new("test").unwrap(),
        }
    }

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[test]
    fn forbidden_capabilities_cannot_be_advertised_as_available() {
        let doc = CapabilityDocument::new(
            host(),
            at(),
            BoundaryLimits::default(),
            [
                CapabilityDescriptor {
                    id: CapabilityId::ComputerControl,
                    since: ContractVersion::new(1, 0),
                    availability: Availability::Available,
                },
                CapabilityDescriptor {
                    id: CapabilityId::ProviderCredentials,
                    since: ContractVersion::new(1, 0),
                    availability: Availability::Available,
                },
            ],
        );
        for id in CapabilityId::permanently_forbidden() {
            assert!(!doc.is_available(id), "{id} must never be available");
            let err = doc.require(id).unwrap_err();
            assert_eq!(err.code, SdkErrorCode::ForbiddenScope);
        }
    }

    #[test]
    fn unadvertised_and_denied_are_distinct_failures() {
        let doc = CapabilityDocument::new(
            host(),
            at(),
            BoundaryLimits::default(),
            [CapabilityDescriptor {
                id: CapabilityId::ComputerRead,
                since: ContractVersion::new(1, 0),
                availability: Availability::Unsupported {
                    reason: Label::new("no computer-use ledger on this host").unwrap(),
                },
            }],
        );
        assert_eq!(
            doc.require(&CapabilityId::ComputerRead).unwrap_err().code,
            SdkErrorCode::Unsupported
        );
        assert_eq!(
            doc.require(&CapabilityId::RunEventsLive).unwrap_err().code,
            SdkErrorCode::CapabilityUnavailable
        );
    }

    #[test]
    fn capability_ids_round_trip() {
        for id in [
            CapabilityId::SessionCreate,
            CapabilityId::SessionList,
            CapabilityId::TaskSubmit,
            CapabilityId::RunObserve,
            CapabilityId::RunEventsPage,
            CapabilityId::RunEventsLive,
            CapabilityId::RunFollowUp,
            CapabilityId::RunCancel,
            CapabilityId::ControlLease,
            CapabilityId::ArtifactFetch,
            CapabilityId::ComputerRead,
            CapabilityId::ComputerControl,
            CapabilityId::ProviderCredentials,
        ] {
            assert_eq!(CapabilityId::from_wire(id.as_wire()), id);
        }
        assert_eq!(
            CapabilityId::from_wire("future.thing").as_wire(),
            "future.thing"
        );
    }
}
