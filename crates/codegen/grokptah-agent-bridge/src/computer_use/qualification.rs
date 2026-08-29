//! Deterministic Computer Use packaged-authority acceptance oracle (#444).
//!
//! These fixtures prove the helper contract without OS input. They cannot
//! count as packaged, signed, or hardware qualification.

use serde::{Deserialize, Serialize};

use super::helper_authority::{
    CleanupReceipt, EffectDisposition, HelperCrashCut, HelperSupervisor, HelperWorld,
};
use super::package_identity::{
    EligibilityInput, ExecutorKind, PackagedEligibility, SigningClass, APP_BUNDLE_ID, APP_VERSION,
    HELPER_BUNDLE_ID, HELPER_VERSION, PACKAGE_AUTHORITY_EVIDENCE_SCHEMA,
};
use super::platform::ComputerPermissionStatus;
use super::types::{ComputerAction, ComputerErrorCode, Sensitivity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationScenario {
    PermissionMissing,
    PermissionGranted,
    PermissionRevoked,
    SemanticObserveApproveActReobserve,
    StaleTarget,
    SecureField,
    TakeoverRace,
    HelperCrashBeforeInjection,
    HelperCrashAfterInjectionBeforeReceipt,
    DuplicateDispatchId,
    Restart,
    Cleanup,
}

impl QualificationScenario {
    pub fn all() -> &'static [Self] {
        &[
            Self::PermissionMissing,
            Self::PermissionGranted,
            Self::PermissionRevoked,
            Self::SemanticObserveApproveActReobserve,
            Self::StaleTarget,
            Self::SecureField,
            Self::TakeoverRace,
            Self::HelperCrashBeforeInjection,
            Self::HelperCrashAfterInjectionBeforeReceipt,
            Self::DuplicateDispatchId,
            Self::Restart,
            Self::Cleanup,
        ]
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PermissionMissing => "permission_missing",
            Self::PermissionGranted => "permission_granted",
            Self::PermissionRevoked => "permission_revoked",
            Self::SemanticObserveApproveActReobserve => "semantic_observe_approve_act_reobserve",
            Self::StaleTarget => "stale_target",
            Self::SecureField => "secure_field",
            Self::TakeoverRace => "takeover_race",
            Self::HelperCrashBeforeInjection => "helper_crash_before_injection",
            Self::HelperCrashAfterInjectionBeforeReceipt => {
                "helper_crash_after_injection_before_receipt"
            }
            Self::DuplicateDispatchId => "duplicate_dispatch_id",
            Self::Restart => "restart",
            Self::Cleanup => "cleanup",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioResult {
    pub scenario: QualificationScenario,
    pub passed: bool,
    pub injected: bool,
    pub disposition: Option<EffectDisposition>,
    pub error_code: Option<ComputerErrorCode>,
    pub cleanup: Option<CleanupReceipt>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyntheticOracleReport {
    pub schema: String,
    pub synthetic_contract: bool,
    pub packaged_qualification: bool,
    pub scenarios: Vec<ScenarioResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityVerdict {
    Pass,
    Partial,
    FailClosed,
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
    pub signing_class: SigningClass,
    pub executor_kind: ExecutorKind,
    pub helper_assembled: bool,
    pub real_tcc_hardware_action_ran: bool,
    pub disk_free_gib_milli: u64,
    pub eligibility: PackagedEligibility,
    pub synthetic_oracle: SyntheticOracleReport,
    pub verdict: AuthorityVerdict,
}

impl PackageAuthorityEvidence {
    pub fn assemble(input: EvidenceAssembly<'_>) -> Self {
        let eligibility = PackagedEligibility::evaluate(EligibilityInput {
            disk_free_gib: input.disk_free_gib,
            target_occupied: input.target_occupied,
            signing_class: input.signing_class,
            executor_kind: input.executor_kind,
            helper_assembled: input.helper_assembled,
            screen_recording_granted: input.screen_recording_granted,
            accessibility_granted: input.accessibility_granted,
            real_hardware_action_ran: input.real_tcc_hardware_action_ran,
            simulator_or_fixture_only: true,
        });
        let synthetic_ok = input
            .synthetic_oracle
            .scenarios
            .iter()
            .all(|scenario| scenario.passed);
        let verdict = if eligibility.packaged_qualification
            && synthetic_ok
            && input.real_tcc_hardware_action_ran
        {
            AuthorityVerdict::Pass
        } else if synthetic_ok {
            AuthorityVerdict::Partial
        } else {
            AuthorityVerdict::FailClosed
        };
        Self {
            schema: PACKAGE_AUTHORITY_EVIDENCE_SCHEMA.to_string(),
            source_head: input.source_head.to_string(),
            branch: input.branch.to_string(),
            app_bundle_id: APP_BUNDLE_ID.to_string(),
            helper_bundle_id: HELPER_BUNDLE_ID.to_string(),
            app_version: APP_VERSION.to_string(),
            helper_version: HELPER_VERSION.to_string(),
            signing_class: input.signing_class,
            executor_kind: input.executor_kind,
            helper_assembled: input.helper_assembled,
            real_tcc_hardware_action_ran: input.real_tcc_hardware_action_ran,
            disk_free_gib_milli: (input.disk_free_gib * 1000.0).max(0.0) as u64,
            eligibility,
            synthetic_oracle: input.synthetic_oracle.clone(),
            verdict,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvidenceAssembly<'a> {
    pub source_head: &'a str,
    pub branch: &'a str,
    pub signing_class: SigningClass,
    pub executor_kind: ExecutorKind,
    pub helper_assembled: bool,
    pub screen_recording_granted: bool,
    pub accessibility_granted: bool,
    pub real_tcc_hardware_action_ran: bool,
    pub disk_free_gib: f64,
    pub target_occupied: bool,
    pub synthetic_oracle: &'a SyntheticOracleReport,
}

pub fn run_synthetic_oracle() -> SyntheticOracleReport {
    let scenarios = QualificationScenario::all()
        .iter()
        .copied()
        .map(run_scenario)
        .collect::<Vec<_>>();
    let synthetic_contract = scenarios.iter().all(|scenario| scenario.passed);
    SyntheticOracleReport {
        schema: PACKAGE_AUTHORITY_EVIDENCE_SCHEMA.to_string(),
        synthetic_contract,
        packaged_qualification: false,
        scenarios,
    }
}

fn run_scenario(scenario: QualificationScenario) -> ScenarioResult {
    match scenario {
        QualificationScenario::PermissionMissing => permission_case(
            scenario,
            ComputerPermissionStatus::Missing,
            ComputerErrorCode::PermissionRequired,
            false,
        ),
        QualificationScenario::PermissionGranted => semantic_success(scenario),
        QualificationScenario::PermissionRevoked => permission_case(
            scenario,
            ComputerPermissionStatus::Revoked,
            ComputerErrorCode::PermissionRevoked,
            false,
        ),
        QualificationScenario::SemanticObserveApproveActReobserve => semantic_success(scenario),
        QualificationScenario::StaleTarget => {
            let mut world = HelperWorld::granted_demo();
            world.live_target.generation = world.target.generation.saturating_add(1);
            deny_without_injection(
                scenario,
                world,
                ComputerErrorCode::TargetChanged,
                "stale target generation denied before injection",
            )
        }
        QualificationScenario::SecureField => {
            let mut world = HelperWorld::granted_demo();
            world.element_sensitivity = Sensitivity::Secure;
            deny_without_injection(
                scenario,
                world,
                ComputerErrorCode::SensitiveSurface,
                "secure field denied before injection",
            )
        }
        QualificationScenario::TakeoverRace => {
            let mut world = HelperWorld::granted_demo();
            world.takeover = true;
            let (supervisor, lease) = launched(world);
            let receipt = supervisor
                .dispatch("dispatch-takeover", &lease, &set_value())
                .expect("takeover receipt");
            passed(
                scenario,
                !receipt.injected
                    && receipt.disposition == EffectDisposition::Cancelled
                    && receipt.cleanup.is_exact(),
                receipt.injected,
                Some(receipt.disposition),
                receipt.error_code,
                Some(receipt.cleanup),
                "operator takeover wins the dispatch race",
            )
        }
        QualificationScenario::HelperCrashBeforeInjection => {
            let mut world = HelperWorld::granted_demo();
            world.crash_cut = Some(HelperCrashCut::BeforeInjection);
            let (supervisor, lease) = launched(world);
            let receipt = supervisor
                .dispatch("dispatch-crash-before", &lease, &set_value())
                .expect("crash-before receipt");
            passed(
                scenario,
                !receipt.injected
                    && receipt.disposition == EffectDisposition::Failed
                    && receipt.cleanup.is_exact()
                    && supervisor.injection_count() == 0,
                receipt.injected,
                Some(receipt.disposition),
                receipt.error_code,
                Some(receipt.cleanup),
                "helper crash before injection leaves zero input",
            )
        }
        QualificationScenario::HelperCrashAfterInjectionBeforeReceipt => {
            let mut world = HelperWorld::granted_demo();
            world.crash_cut = Some(HelperCrashCut::AfterInjectionBeforeReceipt);
            let (supervisor, lease) = launched(world);
            let first = supervisor
                .dispatch("dispatch-crash-after", &lease, &set_value())
                .expect("crash-after receipt");
            let replay = supervisor
                .dispatch("dispatch-crash-after", &lease, &set_value())
                .expect("crash-after replay");
            passed(
                scenario,
                first.injected
                    && first.disposition == EffectDisposition::Uncertain
                    && first == replay
                    && supervisor.injection_count() == 1
                    && first.cleanup.is_exact(),
                first.injected,
                Some(first.disposition),
                first.error_code,
                Some(first.cleanup),
                "helper crash after injection is uncertain and never replayed",
            )
        }
        QualificationScenario::DuplicateDispatchId => {
            let (supervisor, lease) = launched(HelperWorld::granted_demo());
            let first = supervisor
                .dispatch("dispatch-dup", &lease, &set_value())
                .expect("first dispatch");
            let second = supervisor
                .dispatch("dispatch-dup", &lease, &set_value())
                .expect("duplicate dispatch");
            passed(
                scenario,
                first.injected
                    && first == second
                    && supervisor.injection_count() == 1
                    && first.disposition == EffectDisposition::Verified,
                first.injected,
                Some(first.disposition),
                first.error_code,
                Some(first.cleanup),
                "one physical action per dispatch id",
            )
        }
        QualificationScenario::Restart => {
            let (supervisor, lease) = launched(HelperWorld::granted_demo());
            let first = supervisor.recover();
            let second = supervisor.recover();
            let replay = supervisor.dispatch("dispatch-restart", &lease, &set_value());
            passed(
                scenario,
                first.is_exact()
                    && second.is_exact()
                    && supervisor.recoveries() == 2
                    && replay.is_err()
                    && supervisor.injection_count() == 0,
                false,
                None,
                replay.err().map(|error| error.code),
                Some(second),
                "two recovery restarts never replay input",
            )
        }
        QualificationScenario::Cleanup => {
            let (supervisor, lease) = launched(HelperWorld::granted_demo());
            let receipt = supervisor
                .dispatch("dispatch-cleanup", &lease, &set_value())
                .expect("cleanup dispatch");
            let world = supervisor.world();
            passed(
                scenario,
                receipt.cleanup.is_exact()
                    && !world.helper_alive
                    && world.temp_artifacts == 0
                    && receipt.disposition == EffectDisposition::Verified,
                receipt.injected,
                Some(receipt.disposition),
                receipt.error_code,
                Some(receipt.cleanup),
                "success path releases helper, lease, frames, and temp artifacts",
            )
        }
    }
}

fn semantic_success(scenario: QualificationScenario) -> ScenarioResult {
    let (supervisor, lease) = launched(HelperWorld::granted_demo());
    let receipt = supervisor
        .dispatch("dispatch-semantic", &lease, &set_value())
        .expect("semantic dispatch");
    let unchanged_environment =
        receipt.foreground_app == "com.apple.TextEdit" && receipt.pointer == (320, 240);
    passed(
        scenario,
        receipt.injected
            && receipt.disposition == EffectDisposition::Verified
            && receipt.postcondition == Some(true)
            && receipt.cleanup.is_exact()
            && unchanged_environment,
        receipt.injected,
        Some(receipt.disposition),
        receipt.error_code,
        Some(receipt.cleanup),
        "observe-approve-act-reobserve contract: verified postcondition, pointer and foreground unchanged",
    )
}

fn permission_case(
    scenario: QualificationScenario,
    status: ComputerPermissionStatus,
    expected: ComputerErrorCode,
    accessibility_granted: bool,
) -> ScenarioResult {
    let mut world = HelperWorld::granted_demo();
    world.screen_recording = status;
    if !accessibility_granted {
        world.accessibility = status;
    }
    deny_without_injection(
        scenario,
        world,
        expected,
        "permission gate fails closed before injection",
    )
}

fn deny_without_injection(
    scenario: QualificationScenario,
    world: HelperWorld,
    expected: ComputerErrorCode,
    detail: &str,
) -> ScenarioResult {
    let (supervisor, lease) = launched(world);
    let receipt = supervisor
        .dispatch("dispatch-denied", &lease, &set_value())
        .expect("denied receipt");
    passed(
        scenario,
        !receipt.injected
            && receipt.error_code == Some(expected)
            && receipt.cleanup.is_exact()
            && supervisor.injection_count() == 0,
        receipt.injected,
        Some(receipt.disposition),
        receipt.error_code,
        Some(receipt.cleanup),
        detail,
    )
}

fn launched(world: HelperWorld) -> (HelperSupervisor, super::helper_authority::HelperLease) {
    let supervisor = HelperSupervisor::new(world);
    let lease = supervisor
        .attach_synthetic_oracle_session("run-oracle-1", "grant-oracle-1", "observation-oracle-1")
        .expect("synthetic oracle session");
    (supervisor, lease)
}

fn set_value() -> ComputerAction {
    ComputerAction::SetValue {
        element_id: "project-label".into(),
        text: "public-demo-value".into(),
    }
}

fn passed(
    scenario: QualificationScenario,
    ok: bool,
    injected: bool,
    disposition: Option<EffectDisposition>,
    error_code: Option<ComputerErrorCode>,
    cleanup: Option<CleanupReceipt>,
    detail: impl Into<String>,
) -> ScenarioResult {
    ScenarioResult {
        scenario,
        passed: ok,
        injected,
        disposition,
        error_code,
        cleanup,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_oracle_covers_every_required_fixture() {
        let report = run_synthetic_oracle();
        assert_eq!(report.scenarios.len(), QualificationScenario::all().len());
        for scenario in &report.scenarios {
            assert!(
                scenario.passed,
                "{:?} failed: {}",
                scenario.scenario, scenario.detail
            );
        }
        assert!(report.synthetic_contract);
        assert!(!report.packaged_qualification);
    }

    #[test]
    fn evidence_assembly_cannot_promote_synthetic_to_packaged_pass() {
        let oracle = run_synthetic_oracle();
        let evidence = PackageAuthorityEvidence::assemble(EvidenceAssembly {
            source_head: "67e29bd34dc64049432c715c93c2cef2185c63ea",
            branch: "grok/cu-macos-packaged-authority-v1",
            signing_class: SigningClass::AdHoc,
            executor_kind: ExecutorKind::InProcessHost,
            helper_assembled: false,
            screen_recording_granted: false,
            accessibility_granted: false,
            real_tcc_hardware_action_ran: false,
            disk_free_gib: 5.6,
            target_occupied: false,
            synthetic_oracle: &oracle,
        });
        assert_eq!(evidence.verdict, AuthorityVerdict::Partial);
        assert!(!evidence.eligibility.packaged_qualification);
        assert!(!evidence.real_tcc_hardware_action_ran);
        assert_eq!(evidence.app_bundle_id, APP_BUNDLE_ID);
        assert_eq!(evidence.helper_bundle_id, HELPER_BUNDLE_ID);
        assert!(evidence
            .eligibility
            .reasons
            .iter()
            .any(|reason| reason.contains("signing_class_ad_hoc")));
        assert!(evidence
            .eligibility
            .reasons
            .iter()
            .any(|reason| reason.contains("disk_below_20_gib")));
    }
}
