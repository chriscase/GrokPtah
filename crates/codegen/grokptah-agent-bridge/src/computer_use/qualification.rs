//! Deterministic acceptance oracle for the Computer Use packaged authority.
//!
//! Every scenario here drives the *single* host authority in
//! `grokptah-isolated-visual`. There is no second supervisor with its own
//! lease id, dispatch map, or receipt, because two authorities can disagree and
//! then neither is trustworthy about whether input reached the guest.
//!
//! # What this oracle can and cannot establish
//!
//! It runs against the deterministic simulator with a test clock. It proves the
//! state machine, the identity fences, the de-duplication rule, and the cleanup
//! accounting. It cannot and does not establish TCC grants, notarization,
//! Virtualization.framework behavior, guest boot, frames, real input, hardware,
//! or soak. [`AuthorityVerdict`] tops out at [`AuthorityVerdict::Partial`] for
//! exactly that reason.

use std::sync::Arc;

use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};

use grokptah_isolated_visual::{
    ids::{sha256_hex, ISOLATED_VISUAL_BACKEND_ID, SCHEMA_VERSION},
    manifest::{IsolatedSourceEntry, SourceObject, SourceObjectKind},
    protocol::{IsolatedInputEvent, IsolatedInputKind},
    CleanupOutcome, ComputerDispatchState, ComputerSurfaceLeaseState, ContentAddressedStore,
    CreateGuestRequest, HelperIdentity, HermeticResolver, IsolatedCleanupReason, IsolatedErrorCode,
    IsolatedPreflight, IsolatedSourceManifest, IsolatedVisualHost, IsolatedVisualResourceLimits,
    TestClock,
};

use super::package_identity::{ExecutorKind, SigningClass, PACKAGE_AUTHORITY_EVIDENCE_SCHEMA};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationScenario {
    LeaseGrantAndDispatch,
    StaleLeaseRevision,
    ForgedSurfaceIncarnation,
    DuplicateDispatchIdenticalPayload,
    DuplicateDispatchChangedPayload,
    CrashAfterInjectionIsUncertain,
    RestartDoesNotReplay,
    ExpiredLeaseIsReaped,
    SecondAgentIsRefused,
    CleanupIsExact,
    CleanupFailureIsUncertain,
    ProductionAdmissionDenies,
}

impl QualificationScenario {
    pub fn all() -> &'static [Self] {
        &[
            Self::LeaseGrantAndDispatch,
            Self::StaleLeaseRevision,
            Self::ForgedSurfaceIncarnation,
            Self::DuplicateDispatchIdenticalPayload,
            Self::DuplicateDispatchChangedPayload,
            Self::CrashAfterInjectionIsUncertain,
            Self::RestartDoesNotReplay,
            Self::ExpiredLeaseIsReaped,
            Self::SecondAgentIsRefused,
            Self::CleanupIsExact,
            Self::CleanupFailureIsUncertain,
            Self::ProductionAdmissionDenies,
        ]
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LeaseGrantAndDispatch => "lease_grant_and_dispatch",
            Self::StaleLeaseRevision => "stale_lease_revision",
            Self::ForgedSurfaceIncarnation => "forged_surface_incarnation",
            Self::DuplicateDispatchIdenticalPayload => "duplicate_dispatch_identical_payload",
            Self::DuplicateDispatchChangedPayload => "duplicate_dispatch_changed_payload",
            Self::CrashAfterInjectionIsUncertain => "crash_after_injection_is_uncertain",
            Self::RestartDoesNotReplay => "restart_does_not_replay",
            Self::ExpiredLeaseIsReaped => "expired_lease_is_reaped",
            Self::SecondAgentIsRefused => "second_agent_is_refused",
            Self::CleanupIsExact => "cleanup_is_exact",
            Self::CleanupFailureIsUncertain => "cleanup_failure_is_uncertain",
            Self::ProductionAdmissionDenies => "production_admission_denies",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioResult {
    pub scenario: QualificationScenario,
    pub passed: bool,
    pub detail: String,
}

impl ScenarioResult {
    fn pass(scenario: QualificationScenario, detail: impl Into<String>) -> Self {
        Self {
            scenario,
            passed: true,
            detail: detail.into(),
        }
    }

    fn fail(scenario: QualificationScenario, detail: impl Into<String>) -> Self {
        Self {
            scenario,
            passed: false,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyntheticOracleReport {
    pub schema: String,
    /// Always true. This report describes a synthetic contract, never hardware.
    pub synthetic_contract: bool,
    /// Always false, for the same reason.
    pub packaged_qualification: bool,
    pub scenarios: Vec<ScenarioResult>,
}

impl SyntheticOracleReport {
    pub fn all_passed(&self) -> bool {
        self.scenarios.iter().all(|scenario| scenario.passed)
    }
}

/// How far the evidence actually reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityVerdict {
    /// The inputs needed to decide were not present on this host.
    Unavailable,
    /// Inputs were present and admission denied.
    FailClosed,
    /// The synthetic contract holds; hardware and signing evidence are absent.
    Partial,
    /// Reserved. Requires observed TCC grants, notarization, and a real
    /// hardware action, none of which this crate can produce.
    Pass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageAuthorityEvidence {
    pub schema: String,
    pub source_head: String,
    pub branch: String,
    pub app_bundle_id: String,
    pub helper_bundle_id: String,
    pub app_version: String,
    pub helper_version: String,
    pub executor_kind: ExecutorKind,
    pub signing_class: SigningClass,
    /// Observed, not asserted. False everywhere in this repository today.
    pub helper_assembled: bool,
    pub tcc_grants_observed: bool,
    pub notarization_observed: bool,
    pub virtualization_framework_observed: bool,
    pub real_hardware_action_observed: bool,
    pub soak_observed: bool,
    pub preflight_deny_reasons: Vec<String>,
    pub synthetic_oracle: SyntheticOracleReport,
    pub verdict: AuthorityVerdict,
    /// Plain-language statement of what the verdict does not cover.
    pub nonqualification: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct EvidenceAssembly {
    pub source_head: String,
    pub branch: String,
    pub synthetic_oracle: SyntheticOracleReport,
    pub preflight: IsolatedPreflight,
}

impl PackageAuthorityEvidence {
    /// Assemble a verdict from what was actually observed.
    ///
    /// The ceiling is deliberate: without an admitted signed helper this is
    /// `Unavailable` or `FailClosed`, and even with one it stops at `Partial`
    /// because nothing here observes hardware.
    pub fn assemble(input: EvidenceAssembly) -> Self {
        let preflight = &input.preflight;
        let synthetic_ok = input.synthetic_oracle.all_passed();
        let helper = preflight.helper_identity.as_ref();
        let signing_class = helper
            .map(|identity| identity.signing_class)
            .unwrap_or_default();
        let executor_kind = if helper.is_some() {
            ExecutorKind::PackagedHelper
        } else {
            ExecutorKind::InProcessHost
        };

        let verdict = if !synthetic_ok {
            AuthorityVerdict::FailClosed
        } else if !preflight.code_identity_probe_available || !preflight.trust_root_present {
            // We could not even ask the questions that decide packaging.
            AuthorityVerdict::Unavailable
        } else if !preflight.launch_intent_admitted {
            AuthorityVerdict::FailClosed
        } else {
            // Admitted artifacts, synthetic contract holds, no hardware seen.
            AuthorityVerdict::Partial
        };

        Self {
            schema: PACKAGE_AUTHORITY_EVIDENCE_SCHEMA.to_string(),
            source_head: input.source_head,
            branch: input.branch,
            app_bundle_id: super::package_identity::APP_BUNDLE_ID.to_string(),
            helper_bundle_id: super::package_identity::HELPER_BUNDLE_ID.to_string(),
            app_version: super::package_identity::APP_VERSION.to_string(),
            helper_version: super::package_identity::HELPER_VERSION.to_string(),
            executor_kind,
            signing_class,
            helper_assembled: helper.is_some(),
            tcc_grants_observed: false,
            notarization_observed: signing_class.counts_as_packaged_release(),
            virtualization_framework_observed: preflight.virtualization_framework_launched_claim(),
            real_hardware_action_observed: false,
            soak_observed: false,
            preflight_deny_reasons: preflight
                .deny_reasons
                .iter()
                .map(|reason| format!("{}: {}", reason.category, reason.detail))
                .collect(),
            synthetic_oracle: input.synthetic_oracle,
            verdict,
            nonqualification: vec![
                "TCC Screen Recording / Accessibility grants were not observed".into(),
                "no macOS notarization or stapling was performed by this run".into(),
                "Virtualization.framework was not launched and no guest was booted".into(),
                "no frames were captured and no OS input was dispatched".into(),
                "no hardware action and no soak run were performed".into(),
                "simulator and fixture evidence are ineligible for packaged qualification".into(),
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// The oracle itself
// ---------------------------------------------------------------------------

struct Fixture {
    _dir: tempfile::TempDir,
    clock: Arc<TestClock>,
    host: IsolatedVisualHost,
}

fn source_manifest() -> IsolatedSourceManifest {
    let mut store = ContentAddressedStore::new();
    let body = b"int main(void) { return 0; }\n";
    let digest = store.insert(body);
    IsolatedSourceManifest {
        schema_version: SCHEMA_VERSION,
        backend_id: ISOLATED_VISUAL_BACKEND_ID.into(),
        guest_protocol_version: 1,
        objects: vec![IsolatedSourceEntry {
            relative_path: "guest-init.c".into(),
            object: SourceObject {
                digest_sha256: digest,
                kind: SourceObjectKind::Blob,
                media_type: "text/x-c".into(),
                byte_len: body.len() as u64,
            },
        }],
        helper_content_sha256: sha256_hex(b"helper"),
        helper_signing_requirement_sha256: sha256_hex(b"requirement"),
        guest_image_sha256: None,
        configuration_sha256: sha256_hex(b"configuration"),
    }
}

fn request(tag: &str) -> CreateGuestRequest {
    CreateGuestRequest {
        run_id: format!("run-{tag}"),
        work_id: format!("work-{tag}"),
        work_attempt_id: format!("attempt-{tag}"),
        agent_id: format!("agent-{tag}"),
        agent_spec_revision: 1,
        helper: HelperIdentity {
            helper_id: format!("helper-{tag}"),
            content_sha256: sha256_hex(b"helper"),
            signing_requirement_sha256: sha256_hex(b"requirement"),
        },
        source: source_manifest(),
        limits: IsolatedVisualResourceLimits::proof_defaults(),
    }
}

fn open_host(root: &std::path::Path, clock: &Arc<TestClock>) -> IsolatedVisualHost {
    IsolatedVisualHost::open_with_preflight(
        root,
        clock.clone(),
        HermeticResolver::new(ContentAddressedStore::new()),
        IsolatedPreflight::denied("acceptance oracle: no packaged artifacts"),
    )
    .expect("host opens")
}

fn fixture() -> Fixture {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let clock = Arc::new(TestClock::new(Utc::now()));
    let host = open_host(&dir.path().join("store"), &clock);
    Fixture {
        _dir: dir,
        clock,
        host,
    }
}

/// Drive one guest to Running with a granted lease and one frame.
fn ready_guest(fixture: &mut Fixture, tag: &str) -> (String, String) {
    let guest = fixture.host.create_guest(request(tag)).expect("guest");
    fixture.clock.jump(Duration::seconds(1));
    fixture.host.mark_ready(&guest.guest_id).expect("ready");
    fixture.clock.jump(Duration::seconds(1));
    fixture.host.mark_running(&guest.guest_id).expect("running");
    let lease = fixture
        .host
        .enqueue_lease(&guest.guest_id)
        .expect("enqueue");
    fixture
        .host
        .grant_next(&guest.conflict_domain_id)
        .expect("grant");
    fixture
        .host
        .ingest_frame(&guest.guest_id, &lease.lease_id, 8, 8, b"frame")
        .expect("frame");
    (guest.guest_id, lease.lease_id)
}

fn event(
    fixture: &Fixture,
    guest_id: &str,
    lease_id: &str,
    dispatch_id: &str,
    key: &str,
) -> IsolatedInputEvent {
    let guest = fixture.host.guest(guest_id).expect("guest");
    let lease = fixture
        .host
        .leases()
        .expect("leases")
        .into_iter()
        .find(|lease| lease.lease_id == lease_id)
        .expect("lease");
    IsolatedInputEvent {
        dispatch_id: dispatch_id.into(),
        guest_id: guest_id.into(),
        lease_id: lease_id.into(),
        lease_revision: lease.revision,
        surface_id: guest.surface.surface_id.clone(),
        incarnation: guest.surface.incarnation.clone(),
        frame_epoch: guest.frame_epoch,
        kind: IsolatedInputKind::Key {
            code: key.into(),
            pressed: true,
        },
    }
}

/// Run every scenario against the single host authority.
pub fn run_synthetic_oracle() -> SyntheticOracleReport {
    let scenarios = QualificationScenario::all()
        .iter()
        .map(|scenario| run_scenario(*scenario))
        .collect();
    SyntheticOracleReport {
        schema: PACKAGE_AUTHORITY_EVIDENCE_SCHEMA.to_string(),
        synthetic_contract: true,
        // Synthetic evidence is never packaged qualification, whatever passes.
        packaged_qualification: false,
        scenarios,
    }
}

fn run_scenario(scenario: QualificationScenario) -> ScenarioResult {
    use QualificationScenario as S;
    match scenario {
        S::LeaseGrantAndDispatch => {
            let mut fixture = fixture();
            let (guest_id, lease_id) = ready_guest(&mut fixture, "a");
            let event = event(&fixture, &guest_id, &lease_id, "dispatch-1", "a");
            match fixture
                .host
                .inject_dispatch(&guest_id, &lease_id, event, false)
            {
                Ok(lease)
                    if lease.state == ComputerSurfaceLeaseState::Released
                        && lease.dispatch.as_ref().map(|d| d.state)
                            == Some(ComputerDispatchState::Acknowledged) =>
                {
                    ScenarioResult::pass(scenario, "granted lease dispatched and acknowledged once")
                }
                Ok(lease) => {
                    ScenarioResult::fail(scenario, format!("unexpected {:?}", lease.state))
                }
                Err(error) => ScenarioResult::fail(scenario, error.message),
            }
        }
        S::StaleLeaseRevision => {
            let mut fixture = fixture();
            let (guest_id, lease_id) = ready_guest(&mut fixture, "a");
            let mut stale = event(&fixture, &guest_id, &lease_id, "dispatch-1", "a");
            stale.lease_revision = stale.lease_revision.saturating_add(7);
            expect_denied(
                scenario,
                &mut fixture,
                &guest_id,
                &lease_id,
                stale,
                &[IsolatedErrorCode::StaleObservation],
            )
        }
        S::ForgedSurfaceIncarnation => {
            let mut fixture = fixture();
            let (guest_id, lease_id) = ready_guest(&mut fixture, "a");
            let mut forged = event(&fixture, &guest_id, &lease_id, "dispatch-1", "a");
            forged.incarnation = "forged-incarnation".into();
            expect_denied(
                scenario,
                &mut fixture,
                &guest_id,
                &lease_id,
                forged,
                &[IsolatedErrorCode::Unauthorized],
            )
        }
        S::DuplicateDispatchIdenticalPayload => {
            let mut fixture = fixture();
            let (guest_id, lease_id) = ready_guest(&mut fixture, "a");
            let event = event(&fixture, &guest_id, &lease_id, "dispatch-1", "a");
            let _ = fixture
                .host
                .inject_dispatch(&guest_id, &lease_id, event.clone(), false);
            let _ = fixture
                .host
                .inject_dispatch(&guest_id, &lease_id, event, false);
            let injected = fixture.host.simulator().input_len(&guest_id);
            if injected == 1 {
                ScenarioResult::pass(scenario, "identical replay injected exactly once")
            } else {
                ScenarioResult::fail(scenario, format!("injected {injected} times"))
            }
        }
        S::DuplicateDispatchChangedPayload => {
            let mut fixture = fixture();
            let (guest_id, lease_id) = ready_guest(&mut fixture, "a");
            let first = event(&fixture, &guest_id, &lease_id, "dispatch-1", "a");
            let _ = fixture
                .host
                .inject_dispatch(&guest_id, &lease_id, first.clone(), false);
            let mut changed = first;
            changed.kind = IsolatedInputKind::Key {
                code: "z".into(),
                pressed: true,
            };
            match fixture
                .host
                .inject_dispatch(&guest_id, &lease_id, changed, false)
            {
                Err(error) if error.code == IsolatedErrorCode::Conflict => {
                    ScenarioResult::pass(scenario, "reused dispatch id with new payload refused")
                }
                Err(error) => {
                    ScenarioResult::fail(scenario, format!("wrong code {:?}", error.code))
                }
                Ok(_) => ScenarioResult::fail(scenario, "changed payload was accepted"),
            }
        }
        S::CrashAfterInjectionIsUncertain => {
            let mut fixture = fixture();
            let (guest_id, lease_id) = ready_guest(&mut fixture, "a");
            let event = event(&fixture, &guest_id, &lease_id, "dispatch-1", "a");
            let _ = fixture
                .host
                .inject_dispatch(&guest_id, &lease_id, event.clone(), true);
            // Replaying an already-injected dispatch is refused as uncertain.
            match fixture
                .host
                .inject_dispatch(&guest_id, &lease_id, event, false)
            {
                Err(error) if error.code == IsolatedErrorCode::UncertainOutcome => {
                    ScenarioResult::pass(scenario, "injected-then-crashed dispatch is uncertain")
                }
                Err(error) => {
                    ScenarioResult::fail(scenario, format!("wrong code {:?}", error.code))
                }
                Ok(_) => ScenarioResult::fail(scenario, "uncertain dispatch was replayed"),
            }
        }
        S::RestartDoesNotReplay => {
            let dir = tempfile::TempDir::new().expect("tempdir");
            let clock = Arc::new(TestClock::new(Utc::now()));
            let root = dir.path().join("store");
            let lease_id = {
                let mut fixture = Fixture {
                    _dir: tempfile::TempDir::new().expect("tempdir"),
                    clock: clock.clone(),
                    host: open_host(&root, &clock),
                };
                let (guest_id, lease_id) = ready_guest(&mut fixture, "a");
                let event = event(&fixture, &guest_id, &lease_id, "dispatch-1", "a");
                let _ = fixture
                    .host
                    .inject_dispatch(&guest_id, &lease_id, event, true);
                lease_id
            };
            let mut detail = String::new();
            for restart in 1..=2 {
                clock.jump(Duration::seconds(1));
                let host = open_host(&root, &clock);
                let lease = host
                    .leases()
                    .expect("leases")
                    .into_iter()
                    .find(|lease| lease.lease_id == lease_id);
                match lease.map(|lease| lease.state) {
                    Some(ComputerSurfaceLeaseState::Uncertain) => {}
                    other => {
                        detail = format!("restart {restart}: {other:?}");
                        break;
                    }
                }
            }
            if detail.is_empty() {
                ScenarioResult::pass(scenario, "two restarts kept the dispatch Uncertain")
            } else {
                ScenarioResult::fail(scenario, detail)
            }
        }
        S::ExpiredLeaseIsReaped => {
            let mut fixture = fixture();
            let (guest_id, lease_id) = ready_guest(&mut fixture, "a");
            let event = event(&fixture, &guest_id, &lease_id, "dispatch-1", "a");
            fixture.clock.jump(Duration::minutes(30));
            let denied = fixture
                .host
                .prepare_dispatch(&guest_id, &lease_id, event)
                .is_err();
            let terminal = fixture
                .host
                .leases()
                .expect("leases")
                .into_iter()
                .find(|lease| lease.lease_id == lease_id)
                .is_some_and(|lease| lease.state.is_terminal());
            if denied && terminal {
                ScenarioResult::pass(scenario, "expired lease reaped and refused")
            } else {
                ScenarioResult::fail(scenario, format!("denied={denied} terminal={terminal}"))
            }
        }
        S::SecondAgentIsRefused => {
            let mut fixture = fixture();
            let (guest_id, _lease_id) = ready_guest(&mut fixture, "a");
            match fixture.host.enqueue_lease(&guest_id) {
                Err(error) if error.code == IsolatedErrorCode::Conflict => {
                    ScenarioResult::pass(scenario, "a leased guest refuses a second lease")
                }
                Err(error) => {
                    ScenarioResult::fail(scenario, format!("wrong code {:?}", error.code))
                }
                Ok(_) => ScenarioResult::fail(scenario, "a second lease was granted"),
            }
        }
        S::CleanupIsExact => {
            let mut fixture = fixture();
            let (guest_id, _) = ready_guest(&mut fixture, "a");
            fixture.clock.jump(Duration::seconds(1));
            let _ = fixture
                .host
                .terminate(&guest_id, IsolatedCleanupReason::Success);
            match fixture.host.cleanup(&guest_id) {
                Ok((guest, receipt))
                    if receipt.outcome == CleanupOutcome::Exact && guest.cleaned =>
                {
                    ScenarioResult::pass(
                        scenario,
                        "every resource independently confirmed released",
                    )
                }
                Ok((_, receipt)) => ScenarioResult::fail(scenario, receipt.unresolved.join("; ")),
                Err(error) => ScenarioResult::fail(scenario, error.message),
            }
        }
        S::CleanupFailureIsUncertain => {
            let mut fixture = fixture();
            let (guest_id, _) = ready_guest(&mut fixture, "a");
            fixture.clock.jump(Duration::seconds(1));
            let _ = fixture
                .host
                .terminate(&guest_id, IsolatedCleanupReason::Success);
            // Make the overlay unlink genuinely fail by replacing the file with
            // a non-empty directory.
            let overlay = fixture
                .host
                .store_root()
                .join("overlays")
                .join(format!("{guest_id}.overlay"));
            let _ = std::fs::remove_file(&overlay);
            let _ = std::fs::create_dir(&overlay);
            let _ = std::fs::write(overlay.join("occupant"), b"x");
            match fixture.host.cleanup(&guest_id) {
                Ok((guest, receipt))
                    if receipt.outcome == CleanupOutcome::Unresolved && !guest.cleaned =>
                {
                    ScenarioResult::pass(scenario, "failed deletion surfaced as unresolved cleanup")
                }
                Ok(_) => ScenarioResult::fail(scenario, "a failed deletion read as clean"),
                Err(error) => ScenarioResult::fail(scenario, error.message),
            }
        }
        S::ProductionAdmissionDenies => {
            // The production path must be reachable and, absent signed
            // artifacts and a trust root, must deny with stated reasons.
            let preflight = super::isolated_visual::isolated_visual_admission();
            if !preflight.allowed_to_launch
                && !preflight.virtualization_framework_launched_claim()
                && !preflight.deny_reasons.is_empty()
            {
                ScenarioResult::pass(
                    scenario,
                    format!("production admission denied: {}", preflight.deny_summary()),
                )
            } else {
                ScenarioResult::fail(scenario, "production admission did not fail closed")
            }
        }
    }
}

fn expect_denied(
    scenario: QualificationScenario,
    fixture: &mut Fixture,
    guest_id: &str,
    lease_id: &str,
    event: IsolatedInputEvent,
    expected: &[IsolatedErrorCode],
) -> ScenarioResult {
    match fixture
        .host
        .inject_dispatch(guest_id, lease_id, event, false)
    {
        Err(error) if expected.contains(&error.code) => {
            ScenarioResult::pass(scenario, error.message)
        }
        Err(error) => ScenarioResult::fail(scenario, format!("wrong code {:?}", error.code)),
        Ok(_) => ScenarioResult::fail(scenario, "the fence did not deny"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scenario_passes_against_the_single_authority() {
        let report = run_synthetic_oracle();
        let failures: Vec<&ScenarioResult> = report
            .scenarios
            .iter()
            .filter(|scenario| !scenario.passed)
            .collect();
        assert!(
            failures.is_empty(),
            "failing scenarios: {:?}",
            failures
                .iter()
                .map(|scenario| (scenario.scenario.as_str(), scenario.detail.as_str()))
                .collect::<Vec<_>>()
        );
        assert_eq!(report.scenarios.len(), QualificationScenario::all().len());
    }

    #[test]
    fn a_passing_synthetic_oracle_is_never_packaged_qualification() {
        let report = run_synthetic_oracle();
        assert!(report.all_passed());
        assert!(report.synthetic_contract);
        assert!(
            !report.packaged_qualification,
            "synthetic evidence must never read as packaged qualification"
        );

        let evidence = PackageAuthorityEvidence::assemble(EvidenceAssembly {
            source_head: "67e29bd34dc64049432c715c93c2cef2185c63ea".into(),
            branch: "test".into(),
            synthetic_oracle: report,
            preflight: super::super::isolated_visual::isolated_visual_admission(),
        });
        assert!(
            matches!(
                evidence.verdict,
                AuthorityVerdict::Unavailable | AuthorityVerdict::FailClosed
            ),
            "unexpected verdict {:?} on a host with no signed artifacts",
            evidence.verdict
        );
        assert!(!evidence.tcc_grants_observed);
        assert!(!evidence.virtualization_framework_observed);
        assert!(!evidence.real_hardware_action_observed);
        assert!(!evidence.soak_observed);
        assert!(!evidence.nonqualification.is_empty());
    }

    #[test]
    fn a_failing_scenario_forces_fail_closed() {
        let mut report = run_synthetic_oracle();
        report.scenarios[0].passed = false;
        let evidence = PackageAuthorityEvidence::assemble(EvidenceAssembly {
            source_head: "67e29bd34dc64049432c715c93c2cef2185c63ea".into(),
            branch: "test".into(),
            synthetic_oracle: report,
            preflight: super::super::isolated_visual::isolated_visual_admission(),
        });
        assert_eq!(evidence.verdict, AuthorityVerdict::FailClosed);
    }
}
