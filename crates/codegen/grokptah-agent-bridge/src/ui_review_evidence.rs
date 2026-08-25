//! Secret-free evidence contract for recurring expert UI/UX reviews.
//!
//! A polished mockup or a one-time screenshot is not a release proof. This
//! record binds a review to an exact assembled Git revision, a packaged window,
//! the required visual/state matrix, accessibility checks, and disposition of
//! severity-ranked findings. It deliberately stores opaque evidence digests
//! and tracking references rather than screenshots, prompts, or private text.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const UI_REVIEW_EVIDENCE_SCHEMA: &str = "grokptah.ui-review-evidence.v1";
pub const MAX_UI_REVIEW_SURFACES: usize = 64;
pub const MAX_UI_REVIEW_WORKFLOWS: usize = 64;
pub const MAX_UI_REVIEW_STATES: usize = 64;
pub const REQUIRED_UI_REVIEW_STATES: [&str; 10] = [
    "wide/light/empty",
    "wide/dark/loading",
    "narrow/light/success",
    "narrow/dark/error",
    "narrow/light/denied",
    "wide/dark/exhausted",
    "narrow/dark/reconnecting",
    "wide/light/long_text",
    "narrow/light/overflow",
    "wide/dark/permission",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum UiReviewCadence {
    IntegrationWave,
    Periodic,
    Release,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum UiReviewSeverity {
    P0,
    P1,
    P2,
    P3,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiReviewDisposition {
    Fixed,
    Accepted,
    Deferred,
    NotApplicable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UiReviewStateEvidence {
    pub state_key: String,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UiReviewAccessibilityEvidence {
    pub keyboard_complete: bool,
    pub focus_order_visible: bool,
    pub screen_reader_labels_and_status: bool,
    pub contrast: bool,
    pub zoom_and_reflow: bool,
    pub reduced_motion: bool,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UiReviewFinding {
    pub finding_id: String,
    pub severity: UiReviewSeverity,
    pub disposition: UiReviewDisposition,
    pub evidence_digest: String,
    pub tracking_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UiReviewEvidence {
    pub schema: String,
    pub review_id: String,
    pub candidate_sha: String,
    pub reviewer_id: String,
    pub review_tool_id: String,
    pub cadence: UiReviewCadence,
    pub reviewed_at: DateTime<Utc>,
    pub packaged_window: bool,
    pub surfaces: Vec<String>,
    pub workflows: Vec<String>,
    pub states: Vec<UiReviewStateEvidence>,
    pub accessibility: UiReviewAccessibilityEvidence,
    pub findings: Vec<UiReviewFinding>,
    pub secret_free: bool,
    pub claim_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiReviewEvidenceError {
    InvalidField(&'static str),
    UnsupportedSchema,
    MissingState(&'static str),
    DuplicateState,
    DuplicateFinding,
    UnresolvedBlockingFinding,
    NotEligible(&'static str),
}

impl std::fmt::Display for UiReviewEvidenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidField(name) => write!(f, "invalid UI review field: {name}"),
            Self::UnsupportedSchema => write!(f, "unsupported UI review evidence schema"),
            Self::MissingState(state) => write!(f, "UI review is missing state: {state}"),
            Self::DuplicateState => write!(f, "UI review contains a duplicate state"),
            Self::DuplicateFinding => write!(f, "UI review contains a duplicate finding"),
            Self::UnresolvedBlockingFinding => {
                write!(f, "UI review contains an unresolved P0/P1 finding")
            }
            Self::NotEligible(name) => write!(f, "UI review is not eligible: {name}"),
        }
    }
}

impl std::error::Error for UiReviewEvidenceError {}

impl UiReviewEvidence {
    pub fn validate(&self) -> Result<(), UiReviewEvidenceError> {
        if self.schema != UI_REVIEW_EVIDENCE_SCHEMA {
            return Err(UiReviewEvidenceError::UnsupportedSchema);
        }
        if !valid_opaque_id(&self.review_id)
            || !valid_opaque_id(&self.reviewer_id)
            || !valid_opaque_id(&self.review_tool_id)
        {
            return Err(UiReviewEvidenceError::InvalidField("identity"));
        }
        if !valid_sha(&self.candidate_sha) {
            return Err(UiReviewEvidenceError::InvalidField("candidate_sha"));
        }
        if self.reviewed_at.timestamp() < 0 || !self.secret_free {
            return Err(UiReviewEvidenceError::InvalidField("secret_free_or_time"));
        }
        if self.surfaces.is_empty()
            || self.surfaces.len() > MAX_UI_REVIEW_SURFACES
            || self.workflows.is_empty()
            || self.workflows.len() > MAX_UI_REVIEW_WORKFLOWS
            || self.surfaces.iter().any(|value| !valid_opaque_id(value))
            || self.workflows.iter().any(|value| !valid_opaque_id(value))
        {
            return Err(UiReviewEvidenceError::InvalidField("surfaces_or_workflows"));
        }
        if self.states.is_empty() || self.states.len() > MAX_UI_REVIEW_STATES {
            return Err(UiReviewEvidenceError::NotEligible("state cardinality"));
        }
        let mut seen_states = std::collections::BTreeSet::new();
        for state in &self.states {
            if !seen_states.insert(state.state_key.as_str()) {
                return Err(UiReviewEvidenceError::DuplicateState);
            }
            if !valid_state_key(&state.state_key) || !valid_fingerprint(&state.evidence_digest) {
                return Err(UiReviewEvidenceError::InvalidField("state"));
            }
        }
        for required in REQUIRED_UI_REVIEW_STATES {
            if !seen_states.contains(required) {
                return Err(UiReviewEvidenceError::MissingState(required));
            }
        }
        let a11y = &self.accessibility;
        if !(a11y.keyboard_complete
            && a11y.focus_order_visible
            && a11y.screen_reader_labels_and_status
            && a11y.contrast
            && a11y.zoom_and_reflow
            && a11y.reduced_motion
            && valid_fingerprint(&a11y.evidence_digest))
        {
            return Err(UiReviewEvidenceError::NotEligible("accessibility"));
        }
        let mut seen_findings = std::collections::BTreeSet::new();
        for finding in &self.findings {
            if !seen_findings.insert(finding.finding_id.as_str()) {
                return Err(UiReviewEvidenceError::DuplicateFinding);
            }
            if !valid_opaque_id(&finding.finding_id)
                || !valid_fingerprint(&finding.evidence_digest)
                || finding
                    .tracking_ref
                    .as_deref()
                    .is_some_and(|value| !valid_opaque_id(value))
            {
                return Err(UiReviewEvidenceError::InvalidField("finding"));
            }
            if matches!(
                finding.severity,
                UiReviewSeverity::P0 | UiReviewSeverity::P1
            ) && !matches!(
                finding.disposition,
                UiReviewDisposition::Fixed | UiReviewDisposition::Accepted
            ) {
                return Err(UiReviewEvidenceError::UnresolvedBlockingFinding);
            }
            if finding.disposition == UiReviewDisposition::Deferred
                && finding.tracking_ref.is_none()
            {
                return Err(UiReviewEvidenceError::InvalidField("tracking_ref"));
            }
        }
        if self.claim_eligible && !self.packaged_window {
            return Err(UiReviewEvidenceError::NotEligible("packaged window"));
        }
        Ok(())
    }

    pub fn certification_ready(&self) -> bool {
        self.claim_eligible && self.validate().is_ok()
    }
}

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_state_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(claim_eligible: bool) -> UiReviewEvidence {
        UiReviewEvidence {
            schema: UI_REVIEW_EVIDENCE_SCHEMA.into(),
            review_id: "ui-review-1".into(),
            candidate_sha: "a".repeat(40),
            reviewer_id: "expert-1".into(),
            review_tool_id: "claude-code".into(),
            cadence: UiReviewCadence::IntegrationWave,
            reviewed_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            packaged_window: true,
            surfaces: vec!["lane-home".into(), "computer-cockpit".into()],
            workflows: vec![
                "review-and-approve".into(),
                "recover-after-reconnect".into(),
            ],
            states: REQUIRED_UI_REVIEW_STATES
                .into_iter()
                .map(|state_key| UiReviewStateEvidence {
                    state_key: state_key.into(),
                    evidence_digest: "b".repeat(64),
                })
                .collect(),
            accessibility: UiReviewAccessibilityEvidence {
                keyboard_complete: true,
                focus_order_visible: true,
                screen_reader_labels_and_status: true,
                contrast: true,
                zoom_and_reflow: true,
                reduced_motion: true,
                evidence_digest: "c".repeat(64),
            },
            findings: vec![UiReviewFinding {
                finding_id: "finding-1".into(),
                severity: UiReviewSeverity::P2,
                disposition: UiReviewDisposition::Deferred,
                evidence_digest: "d".repeat(64),
                tracking_ref: Some("issue-308".into()),
            }],
            secret_free: true,
            claim_eligible,
        }
    }

    #[test]
    fn complete_packaged_review_is_ready_and_secret_free() {
        let report = evidence(true);
        report.validate().unwrap();
        assert!(report.certification_ready());
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("Bearer"));
        assert!(!encoded.contains("api_key"));
    }

    #[test]
    fn missing_matrix_or_unresolved_blocker_fails_closed() {
        let mut missing = evidence(true);
        missing.states.pop();
        assert!(matches!(
            missing.validate(),
            Err(UiReviewEvidenceError::MissingState(_))
        ));

        let mut blocker = evidence(true);
        blocker.findings[0].severity = UiReviewSeverity::P1;
        blocker.findings[0].disposition = UiReviewDisposition::Deferred;
        assert_eq!(
            blocker.validate(),
            Err(UiReviewEvidenceError::UnresolvedBlockingFinding)
        );
    }

    #[test]
    fn non_packaged_review_cannot_claim_release() {
        let mut report = evidence(true);
        report.packaged_window = false;
        assert_eq!(
            report.validate(),
            Err(UiReviewEvidenceError::NotEligible("packaged window"))
        );
    }

    #[test]
    fn unknown_fields_bad_digest_and_deferred_tracking_are_rejected() {
        let mut value = serde_json::to_value(evidence(true)).unwrap();
        value["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<UiReviewEvidence>(value).is_err());

        let mut bad = evidence(true);
        bad.candidate_sha = "not-a-sha".into();
        assert_eq!(
            bad.validate(),
            Err(UiReviewEvidenceError::InvalidField("candidate_sha"))
        );

        let mut missing_tracking = evidence(true);
        missing_tracking.findings[0].tracking_ref = None;
        assert_eq!(
            missing_tracking.validate(),
            Err(UiReviewEvidenceError::InvalidField("tracking_ref"))
        );
    }
}
