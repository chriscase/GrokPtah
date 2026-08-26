//! Canary and soak machinery for the isolated visual substrate.
//!
//! Two different things are deliberately kept apart here.
//!
//! The **source canary** repeats the substrate's own contract rehearsal and
//! checks that it is stable across iterations. It runs today, on any host, and
//! it proves exactly one thing: the substrate's contracts hold repeatedly and
//! identically in this build. It is not a hardware result and its own type name
//! says so.
//!
//! The **hardware soak** is planning only. Starting one requires
//! [`HardwareGateEvidence`], which cannot be built without a measured launch
//! that actually reached stop and reap, plus both independent reviews. On this
//! branch no such launch can exist — the real harness refuses before it reaches
//! a guest — so a soak cannot be started here at all. That is structural, not a
//! flag: there is no argument a caller can pass to get one.

use std::time::Duration;

use super::isolated_visual_harness::{run_real_measured_launch, MeasuredLaunchReport};
use super::isolated_visual_selfcheck::run_isolated_visual_selfcheck;

/// The shortest canary worth running, and the default.
pub(crate) const CANARY_ITERATIONS: u32 = 32;
/// The soak this substrate would eventually have to pass. Named so the refusal
/// below is explicit about what it is refusing.
pub(crate) const QUALIFYING_SOAK: Duration = Duration::from_secs(72 * 60 * 60);

/// What a canary actually measured. The variant name travels with the result so
/// a source run can never be read as a hardware run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CanaryKind {
    /// Repeated in-memory contract rehearsal. No guest, helper, or package.
    SourceContract,
    /// Repeated measured launch on signed hardware. Never produced here.
    MeasuredHardware,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanaryReport {
    pub(crate) kind: CanaryKind,
    pub(crate) iterations: u32,
    pub(crate) stable: bool,
}

impl CanaryReport {
    /// Always false for a source canary, whatever its iteration count. A
    /// hardware claim needs hardware.
    pub(crate) fn is_hardware_evidence(&self) -> bool {
        self.kind == CanaryKind::MeasuredHardware && self.stable && self.iterations > 0
    }
}

/// Repeats the substrate contract rehearsal and reports whether every
/// iteration agreed. Deterministic and allocation-only: no process, descriptor,
/// filesystem, or network work, so it is safe to repeat anywhere.
pub(crate) fn run_source_canary(iterations: u32) -> CanaryReport {
    let mut stable = iterations > 0;
    for _ in 0..iterations {
        if run_isolated_visual_selfcheck().is_err() {
            stable = false;
            break;
        }
    }
    CanaryReport {
        kind: CanaryKind::SourceContract,
        iterations,
        stable,
    }
}

/// A completed independent review. Producing one is a human act recorded
/// outside this crate; the type exists so the soak gate has to name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IndependentReview {
    _private: (),
}

impl IndependentReview {
    #[cfg(test)]
    pub(crate) fn recorded_for_tests() -> Self {
        Self { _private: () }
    }
}

/// Proof that every signed-hardware gate has passed.
///
/// The only way to obtain one is a measured launch that reached stop and reap
/// together with both independent reviews. Nothing on this branch can produce
/// such a launch, so nothing on this branch can produce this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HardwareGateEvidence {
    _private: (),
}

impl HardwareGateEvidence {
    pub(crate) fn from_measured_launch(
        report: &MeasuredLaunchReport,
        _security_review: IndependentReview,
        _accessibility_review: IndependentReview,
    ) -> Option<Self> {
        report
            .proves_hardware_launch()
            .then_some(Self { _private: () })
    }
}

/// Why a soak was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SoakRefusal {
    /// No measured launch has reached stop and reap on signed hardware.
    HardwareGatesUnmet,
    /// The requested duration is not a soak.
    DurationNotPositive,
}

/// A soak that is permitted to start. Holding one is the permission; there is
/// no way to construct it without [`HardwareGateEvidence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PermittedSoak {
    pub(crate) duration: Duration,
}

/// Plans a soak. Planning is always allowed and starts nothing.
pub(crate) fn plan_soak(duration: Duration) -> Result<Duration, SoakRefusal> {
    if duration.is_zero() {
        return Err(SoakRefusal::DurationNotPositive);
    }
    Ok(duration)
}

/// Starts a soak, if and only if the signed-hardware gates have passed.
pub(crate) fn start_soak(
    duration: Duration,
    evidence: Option<HardwareGateEvidence>,
) -> Result<PermittedSoak, SoakRefusal> {
    let duration = plan_soak(duration)?;
    match evidence {
        Some(_) => Ok(PermittedSoak { duration }),
        None => Err(SoakRefusal::HardwareGatesUnmet),
    }
}

/// Whether a soak could be started on this host right now, asked without an
/// operator opt-in so nothing is attempted in order to answer.
pub(crate) fn hardware_soak_startable() -> bool {
    let report = run_real_measured_launch(None);
    report.proves_hardware_launch()
}

#[cfg(test)]
mod tests {
    use super::super::isolated_visual_harness::{LaunchOutcome, LaunchStage};
    use super::*;

    fn proving_report() -> MeasuredLaunchReport {
        MeasuredLaunchReport {
            launch_attempted: LaunchStage::StoppedAndReaped,
            outcome: LaunchOutcome::Completed,
            authenticated_frames: 4,
            acknowledged_inputs: 3,
        }
    }

    #[test]
    fn the_source_canary_is_stable_and_repeatable() {
        let report = run_source_canary(CANARY_ITERATIONS);
        assert_eq!(report.kind, CanaryKind::SourceContract);
        assert_eq!(report.iterations, CANARY_ITERATIONS);
        assert!(report.stable, "the substrate contract rehearsal drifted");
        // Repeating the whole canary must give the same answer.
        assert_eq!(run_source_canary(CANARY_ITERATIONS), report);
    }

    #[test]
    fn a_source_canary_is_never_hardware_evidence() {
        for iterations in [1, CANARY_ITERATIONS, 10_000] {
            let report = CanaryReport {
                kind: CanaryKind::SourceContract,
                iterations,
                stable: true,
            };
            assert!(
                !report.is_hardware_evidence(),
                "a source canary of {iterations} iterations read as hardware evidence"
            );
        }
    }

    #[test]
    fn a_soak_cannot_start_on_this_branch() {
        assert!(
            !hardware_soak_startable(),
            "a soak must not be startable without a measured launch"
        );
        assert_eq!(
            start_soak(QUALIFYING_SOAK, None),
            Err(SoakRefusal::HardwareGatesUnmet)
        );
        // The qualifying soak specifically, named, is refused.
        assert_eq!(QUALIFYING_SOAK, Duration::from_secs(72 * 60 * 60));
    }

    #[test]
    fn hardware_evidence_cannot_be_built_from_an_unfinished_launch() {
        for report in [
            MeasuredLaunchReport {
                launch_attempted: LaunchStage::NotAttempted,
                outcome: LaunchOutcome::Refused("inert"),
                authenticated_frames: 0,
                acknowledged_inputs: 0,
            },
            MeasuredLaunchReport {
                launch_attempted: LaunchStage::UnicodeAcknowledged,
                outcome: LaunchOutcome::Completed,
                authenticated_frames: 4,
                acknowledged_inputs: 3,
            },
            MeasuredLaunchReport {
                launch_attempted: LaunchStage::StoppedAndReaped,
                outcome: LaunchOutcome::Uncertain,
                authenticated_frames: 4,
                acknowledged_inputs: 3,
            },
            MeasuredLaunchReport {
                launch_attempted: LaunchStage::StoppedAndReaped,
                outcome: LaunchOutcome::Completed,
                authenticated_frames: 1,
                acknowledged_inputs: 3,
            },
        ] {
            assert!(
                HardwareGateEvidence::from_measured_launch(
                    &report,
                    IndependentReview::recorded_for_tests(),
                    IndependentReview::recorded_for_tests(),
                )
                .is_none(),
                "hardware evidence was built from {:?}",
                report.launch_attempted
            );
        }
    }

    #[test]
    fn a_soak_starts_only_once_every_gate_has_passed() {
        // The positive path, so the refusal above is a gate rather than a stub.
        let evidence = HardwareGateEvidence::from_measured_launch(
            &proving_report(),
            IndependentReview::recorded_for_tests(),
            IndependentReview::recorded_for_tests(),
        )
        .expect("a completed measured launch plus both reviews is the gate");
        assert_eq!(
            start_soak(QUALIFYING_SOAK, Some(evidence)),
            Ok(PermittedSoak {
                duration: QUALIFYING_SOAK
            })
        );
        // Planning never starts anything and is always allowed.
        assert_eq!(plan_soak(QUALIFYING_SOAK), Ok(QUALIFYING_SOAK));
        assert_eq!(
            plan_soak(Duration::ZERO),
            Err(SoakRefusal::DurationNotPositive)
        );
    }
}
