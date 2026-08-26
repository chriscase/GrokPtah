//! Measured launch harness for the isolated visual substrate.
//!
//! This is the only thing that would ever boot a real guest, and it is
//! deliberately built so that it cannot claim anything it did not measure.
//!
//! Two properties are separated on purpose. The *policy* — stage ordering,
//! fail-closed refusal, freshness and visible-postcondition requirements, and
//! the rule that an uncertain step is never retried — is expressed over a
//! [`MeasuredLaunchSteps`] trait and is therefore fully testable on any host,
//! deterministically and without hardware. The *execution* is a macOS-only
//! implementation over the crate-private packaged supervisor, and it refuses to
//! run unless the operator explicitly opts in on a correctly signed package.
//!
//! Off a signed macOS package the harness is inert: it reports the exact stage
//! it stopped at and why, and it spawns nothing. Dispatch stays disabled; a
//! completed run here still grants no Computer Use authority.

use super::types::{ComputerError, ComputerErrorCode, ComputerResult};

/// How far a measured launch actually got. Ordered: a report may only ever
/// name the furthest stage that was *observed to succeed*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LaunchStage {
    /// Nothing was attempted. No process, descriptor, or bundle was touched.
    NotAttempted,
    /// The host is not a platform this harness can run on.
    PlatformRejected,
    /// A correctly signed package was not established.
    PackageUnverified,
    /// A signed package was discovered, validated, and measured.
    PackageMeasured,
    /// The packaged helper was spawned over the private descriptor topology.
    HelperSpawned,
    /// The guest reported readiness over the authenticated channel.
    GuestBooted,
    /// A complete, authenticated, fresh frame was opened.
    FrameAuthenticated,
    /// Pointer input was acknowledged with a visible postcondition.
    PointerAcknowledged,
    /// Keyboard input was acknowledged with a visible postcondition.
    KeyboardAcknowledged,
    /// Unicode text input was acknowledged with a visible postcondition.
    UnicodeAcknowledged,
    /// The helper was stopped and reaped, with exact cleanup evidence.
    StoppedAndReaped,
}

impl LaunchStage {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NotAttempted => "not_attempted",
            Self::PlatformRejected => "platform_rejected",
            Self::PackageUnverified => "package_unverified",
            Self::PackageMeasured => "package_measured",
            Self::HelperSpawned => "helper_spawned",
            Self::GuestBooted => "guest_booted",
            Self::FrameAuthenticated => "frame_authenticated",
            Self::PointerAcknowledged => "pointer_acknowledged",
            Self::KeyboardAcknowledged => "keyboard_acknowledged",
            Self::UnicodeAcknowledged => "unicode_acknowledged",
            Self::StoppedAndReaped => "stopped_and_reaped",
        }
    }
}

/// Why a measured launch ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchOutcome {
    /// Refused before touching anything.
    Refused(&'static str),
    /// A step failed definitely. The substrate's state is known.
    Failed(ComputerErrorCode),
    /// A step's result is not known. Nothing may be retried and nothing may be
    /// concluded about what the guest did or did not observe.
    Uncertain,
    /// Every stage was measured through stop and reap.
    Completed,
}

/// Secret-free record of one measured launch attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MeasuredLaunchReport {
    /// The furthest stage observed to succeed.
    pub(crate) launch_attempted: LaunchStage,
    pub(crate) outcome: LaunchOutcome,
    /// Frames opened, each authenticated and strictly fresher than the last.
    pub(crate) authenticated_frames: u32,
    /// Inputs acknowledged with a visible postcondition.
    pub(crate) acknowledged_inputs: u32,
}

impl MeasuredLaunchReport {
    fn refused(stage: LaunchStage, reason: &'static str) -> Self {
        Self {
            launch_attempted: stage,
            outcome: LaunchOutcome::Refused(reason),
            authenticated_frames: 0,
            acknowledged_inputs: 0,
        }
    }

    /// A run may be called a hardware proof only if every stage was measured
    /// and nothing was left uncertain.
    pub(crate) fn proves_hardware_launch(&self) -> bool {
        self.outcome == LaunchOutcome::Completed
            && self.launch_attempted == LaunchStage::StoppedAndReaped
            && self.authenticated_frames >= 4
            && self.acknowledged_inputs == 3
    }
}

/// One observed frame. Identity only: frame bytes never reach a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedFrame {
    pub(crate) frame_sequence: u64,
    pub(crate) content_sha256: [u8; 32],
}

/// The steps a real measured launch performs, in order.
///
/// Every method returns a definite result or an error. An error carrying
/// [`ComputerErrorCode::UncertainOutcome`] or [`ComputerErrorCode::Interrupted`]
/// means the step's effect is unknown, which ends the run.
pub(crate) trait MeasuredLaunchSteps {
    /// Establish and measure a correctly signed package.
    fn measure_signed_package(&mut self) -> ComputerResult<()>;
    /// Spawn the packaged helper over the private descriptor topology.
    fn spawn_packaged_helper(&mut self) -> ComputerResult<()>;
    /// Wait for the guest to report readiness over the authenticated channel.
    fn await_guest_boot(&mut self) -> ComputerResult<()>;
    /// Open the next complete authenticated frame.
    fn read_authenticated_frame(&mut self) -> ComputerResult<ObservedFrame>;
    /// Send one pointer event and wait for the guest's acknowledgement.
    fn send_pointer(&mut self) -> ComputerResult<()>;
    /// Send one keyboard event and wait for the guest's acknowledgement.
    fn send_keyboard(&mut self) -> ComputerResult<()>;
    /// Send one Unicode text event and wait for the guest's acknowledgement.
    fn send_unicode_text(&mut self) -> ComputerResult<()>;
    /// Stop the helper, reap it, and establish exact cleanup evidence.
    fn stop_and_reap(&mut self) -> ComputerResult<()>;
}

fn is_uncertain(error: &ComputerError) -> bool {
    matches!(
        error.code,
        ComputerErrorCode::UncertainOutcome | ComputerErrorCode::Interrupted
    )
}

/// Drives one measured launch.
///
/// Every stage must be *observed*, in order. An input counts only when the
/// frame after it is both strictly fresher and visibly different from the frame
/// before it — an acknowledgement alone is not a postcondition. The moment a
/// step's outcome is uncertain the run ends: nothing is retried, no later stage
/// is attempted, and the report says exactly how far it got.
pub(crate) fn run_measured_launch<S: MeasuredLaunchSteps>(steps: &mut S) -> MeasuredLaunchReport {
    let mut report = MeasuredLaunchReport {
        launch_attempted: LaunchStage::NotAttempted,
        outcome: LaunchOutcome::Completed,
        authenticated_frames: 0,
        acknowledged_inputs: 0,
    };

    macro_rules! stage {
        ($call:expr, $reached:expr) => {
            match $call {
                Ok(value) => {
                    report.launch_attempted = $reached;
                    value
                }
                Err(error) => {
                    report.outcome = if is_uncertain(&error) {
                        LaunchOutcome::Uncertain
                    } else {
                        LaunchOutcome::Failed(error.code)
                    };
                    return report;
                }
            }
        };
    }

    stage!(steps.measure_signed_package(), LaunchStage::PackageMeasured);
    stage!(steps.spawn_packaged_helper(), LaunchStage::HelperSpawned);
    stage!(steps.await_guest_boot(), LaunchStage::GuestBooted);

    let mut previous = stage!(
        steps.read_authenticated_frame(),
        LaunchStage::FrameAuthenticated
    );
    report.authenticated_frames = 1;

    for (send, reached) in [
        (0_u8, LaunchStage::PointerAcknowledged),
        (1, LaunchStage::KeyboardAcknowledged),
        (2, LaunchStage::UnicodeAcknowledged),
    ] {
        let sent = match send {
            0 => steps.send_pointer(),
            1 => steps.send_keyboard(),
            _ => steps.send_unicode_text(),
        };
        if let Err(error) = sent {
            report.outcome = if is_uncertain(&error) {
                LaunchOutcome::Uncertain
            } else {
                LaunchOutcome::Failed(error.code)
            };
            return report;
        }

        // The acknowledgement is not the proof. The next frame is.
        let observed = match steps.read_authenticated_frame() {
            Ok(frame) => frame,
            Err(error) => {
                report.outcome = if is_uncertain(&error) {
                    LaunchOutcome::Uncertain
                } else {
                    LaunchOutcome::Failed(error.code)
                };
                return report;
            }
        };
        if observed.frame_sequence <= previous.frame_sequence {
            report.outcome = LaunchOutcome::Failed(ComputerErrorCode::StaleObservation);
            return report;
        }
        report.authenticated_frames += 1;
        if observed.content_sha256 == previous.content_sha256 {
            // The guest acknowledged but nothing visibly changed. That is a
            // failed postcondition, not an uncertain one, and it is not retried.
            report.outcome = LaunchOutcome::Failed(ComputerErrorCode::UncertainOutcome);
            return report;
        }
        previous = observed;
        report.acknowledged_inputs += 1;
        report.launch_attempted = reached;
    }

    stage!(steps.stop_and_reap(), LaunchStage::StoppedAndReaped);
    report
}

/// Operator opt-in for a real measured launch.
///
/// A real launch requires the operator to pass this explicitly. It cannot be
/// produced from configuration, an environment variable alone, a model
/// proposal, or a coordinator request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MeasuredLaunchOptIn {
    _private: (),
}

impl MeasuredLaunchOptIn {
    /// Grants one measured launch attempt on a correctly signed macOS package.
    #[cfg(target_os = "macos")]
    pub(crate) fn granted_by_operator() -> Self {
        Self { _private: () }
    }
}

/// Runs a real measured launch.
///
/// Off macOS this is inert and reports [`LaunchStage::PlatformRejected`]
/// without touching anything. On macOS it requires the operator opt-in and a
/// correctly signed package; the packaged supervisor establishes both, and
/// there is no path that reaches a guest without them.
pub(crate) fn run_real_measured_launch(
    opt_in: Option<MeasuredLaunchOptIn>,
) -> MeasuredLaunchReport {
    let Some(_opt_in) = opt_in else {
        return MeasuredLaunchReport::refused(
            LaunchStage::NotAttempted,
            "a measured launch requires an explicit operator opt-in",
        );
    };

    #[cfg(not(target_os = "macos"))]
    {
        MeasuredLaunchReport::refused(
            LaunchStage::PlatformRejected,
            "the isolated visual harness requires macOS",
        )
    }

    #[cfg(target_os = "macos")]
    {
        // The packaged supervisor is the only way to reach a guest, and it
        // refuses without a correctly signed, measured package. Until the
        // signed-hardware gates and the independent security and accessibility
        // reviews pass, this harness stops here rather than spawning: the
        // supervisor exists and is bound, but nothing in this branch has been
        // measured on real hardware, so there is nothing it could honestly
        // report.
        MeasuredLaunchReport::refused(
            LaunchStage::PackageUnverified,
            "no correctly signed package has been measured on this host",
        )
    }
}

/// A scripted step sequence, used to prove the launch policy without hardware.
///
/// This is not a stand-in for a measured launch and can never produce one: it
/// yields whatever it was scripted with. Its only job is to let the ordering,
/// freshness, visible-postcondition, and never-retry rules above be checked
/// deterministically on any host.
#[derive(Debug, Default)]
pub(crate) struct ScriptedMeasuredLaunch {
    frames: Vec<ComputerResult<ObservedFrame>>,
    measure: Option<ComputerResult<()>>,
    spawn: Option<ComputerResult<()>>,
    boot: Option<ComputerResult<()>>,
    pointer: Option<ComputerResult<()>>,
    keyboard: Option<ComputerResult<()>>,
    unicode: Option<ComputerResult<()>>,
    stop: Option<ComputerResult<()>>,
    calls: Vec<&'static str>,
}

pub(crate) fn observed(sequence: u64, fill: u8) -> ComputerResult<ObservedFrame> {
    Ok(ObservedFrame {
        frame_sequence: sequence,
        content_sha256: [fill; 32],
    })
}

fn scripted_error(code: ComputerErrorCode) -> ComputerResult<()> {
    Err(ComputerError::new(code, "scripted step"))
}

impl ScriptedMeasuredLaunch {
    /// Four strictly fresher, visibly different frames and no failures.
    pub(crate) fn healthy() -> Self {
        Self {
            frames: vec![
                observed(1, 1),
                observed(2, 2),
                observed(3, 3),
                observed(4, 4),
            ],
            ..Self::default()
        }
    }

    pub(crate) fn with_uncertain_spawn(code: ComputerErrorCode) -> Self {
        Self {
            spawn: Some(scripted_error(code)),
            ..Self::healthy()
        }
    }

    pub(crate) fn with_uncertain_keyboard() -> Self {
        Self {
            keyboard: Some(scripted_error(ComputerErrorCode::UncertainOutcome)),
            ..Self::healthy()
        }
    }

    /// The frame after the first input is fresh but visibly identical.
    pub(crate) fn with_invisible_pointer_effect() -> Self {
        Self {
            frames: vec![
                observed(1, 1),
                observed(2, 1),
                observed(3, 3),
                observed(4, 4),
            ],
            ..Self::default()
        }
    }

    /// The frame after the first input does not advance the sequence.
    pub(crate) fn with_stale_frame_after_input() -> Self {
        Self {
            frames: vec![
                observed(4, 1),
                observed(4, 2),
                observed(5, 3),
                observed(6, 4),
            ],
            ..Self::default()
        }
    }

    pub(crate) fn calls(&self) -> &[&'static str] {
        &self.calls
    }

    fn next(&mut self, name: &'static str, slot: Option<ComputerResult<()>>) -> ComputerResult<()> {
        self.calls.push(name);
        slot.unwrap_or(Ok(()))
    }
}

impl MeasuredLaunchSteps for ScriptedMeasuredLaunch {
    fn measure_signed_package(&mut self) -> ComputerResult<()> {
        let slot = self.measure.take();
        self.next("measure", slot)
    }
    fn spawn_packaged_helper(&mut self) -> ComputerResult<()> {
        let slot = self.spawn.take();
        self.next("spawn", slot)
    }
    fn await_guest_boot(&mut self) -> ComputerResult<()> {
        let slot = self.boot.take();
        self.next("boot", slot)
    }
    fn read_authenticated_frame(&mut self) -> ComputerResult<ObservedFrame> {
        self.calls.push("frame");
        if self.frames.is_empty() {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "no scripted frame remains",
            ));
        }
        self.frames.remove(0)
    }
    fn send_pointer(&mut self) -> ComputerResult<()> {
        let slot = self.pointer.take();
        self.next("pointer", slot)
    }
    fn send_keyboard(&mut self) -> ComputerResult<()> {
        let slot = self.keyboard.take();
        self.next("keyboard", slot)
    }
    fn send_unicode_text(&mut self) -> ComputerResult<()> {
        let slot = self.unicode.take();
        self.next("unicode", slot)
    }
    fn stop_and_reap(&mut self) -> ComputerResult<()> {
        let slot = self.stop.take();
        self.next("stop", slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fully_measured_launch_reaches_stop_and_reap() {
        let mut script = ScriptedMeasuredLaunch::healthy();
        let report = run_measured_launch(&mut script);
        assert_eq!(report.outcome, LaunchOutcome::Completed);
        assert_eq!(report.launch_attempted, LaunchStage::StoppedAndReaped);
        assert_eq!(report.authenticated_frames, 4);
        assert_eq!(report.acknowledged_inputs, 3);
        assert!(report.proves_hardware_launch());
        assert_eq!(
            script.calls(),
            vec![
                "measure", "spawn", "boot", "frame", "pointer", "frame", "keyboard", "frame",
                "unicode", "frame", "stop",
            ]
        );
    }

    #[test]
    fn an_uncertain_step_ends_the_run_without_a_retry() {
        for (name, uncertain) in [
            ("spawn", ComputerErrorCode::UncertainOutcome),
            ("spawn", ComputerErrorCode::Interrupted),
        ] {
            let mut script = ScriptedMeasuredLaunch::with_uncertain_spawn(uncertain);
            let report = run_measured_launch(&mut script);
            assert_eq!(report.outcome, LaunchOutcome::Uncertain, "{name}");
            assert_eq!(report.launch_attempted, LaunchStage::PackageMeasured);
            assert!(!report.proves_hardware_launch());
            // The uncertain step was called once and nothing after it ran.
            assert_eq!(script.calls(), vec!["measure", "spawn"]);
        }
    }

    #[test]
    fn an_uncertain_input_is_not_retried_and_proves_nothing() {
        let mut script = ScriptedMeasuredLaunch::with_uncertain_keyboard();
        let report = run_measured_launch(&mut script);
        assert_eq!(report.outcome, LaunchOutcome::Uncertain);
        // Pointer was measured; keyboard was not, and unicode never ran.
        assert_eq!(report.launch_attempted, LaunchStage::PointerAcknowledged);
        assert_eq!(report.acknowledged_inputs, 1);
        assert_eq!(
            script.calls().iter().filter(|c| **c == "keyboard").count(),
            1
        );
        assert!(!script.calls().contains(&"unicode"));
        assert!(!script.calls().contains(&"stop"));
    }

    #[test]
    fn an_acknowledged_input_without_a_visible_change_fails() {
        let mut script = ScriptedMeasuredLaunch::with_invisible_pointer_effect();
        let report = run_measured_launch(&mut script);
        assert_eq!(
            report.outcome,
            LaunchOutcome::Failed(ComputerErrorCode::UncertainOutcome)
        );
        assert_eq!(report.launch_attempted, LaunchStage::FrameAuthenticated);
        assert_eq!(report.acknowledged_inputs, 0);
        assert!(!report.proves_hardware_launch());
    }

    #[test]
    fn a_stale_frame_after_an_input_fails_closed() {
        let mut script = ScriptedMeasuredLaunch::with_stale_frame_after_input();
        let report = run_measured_launch(&mut script);
        assert_eq!(
            report.outcome,
            LaunchOutcome::Failed(ComputerErrorCode::StaleObservation)
        );
        assert!(!report.proves_hardware_launch());
    }

    #[test]
    fn a_partial_run_can_never_be_called_a_hardware_proof() {
        for stage in [
            LaunchStage::NotAttempted,
            LaunchStage::PlatformRejected,
            LaunchStage::PackageUnverified,
            LaunchStage::PackageMeasured,
            LaunchStage::HelperSpawned,
            LaunchStage::GuestBooted,
            LaunchStage::FrameAuthenticated,
            LaunchStage::PointerAcknowledged,
            LaunchStage::KeyboardAcknowledged,
            LaunchStage::UnicodeAcknowledged,
        ] {
            let report = MeasuredLaunchReport {
                launch_attempted: stage,
                outcome: LaunchOutcome::Completed,
                authenticated_frames: 4,
                acknowledged_inputs: 3,
            };
            assert!(
                !report.proves_hardware_launch(),
                "{} must not read as a hardware proof",
                stage.as_str()
            );
        }
    }

    #[test]
    fn the_real_harness_is_inert_without_an_operator_opt_in() {
        let report = run_real_measured_launch(None);
        assert_eq!(report.launch_attempted, LaunchStage::NotAttempted);
        assert!(matches!(report.outcome, LaunchOutcome::Refused(_)));
        assert_eq!(report.authenticated_frames, 0);
        assert_eq!(report.acknowledged_inputs, 0);
        assert!(!report.proves_hardware_launch());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_real_harness_refuses_off_macos_even_with_an_opt_in() {
        // The opt-in constructor does not exist off macOS, so this platform
        // cannot even express the request. The refusal is checked through the
        // only path that remains reachable here.
        let report = run_real_measured_launch(None);
        assert!(matches!(report.outcome, LaunchOutcome::Refused(_)));
        assert!(!report.proves_hardware_launch());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_real_harness_refuses_without_a_measured_signed_package() {
        let report = run_real_measured_launch(Some(MeasuredLaunchOptIn::granted_by_operator()));
        assert_eq!(report.launch_attempted, LaunchStage::PackageUnverified);
        assert!(matches!(report.outcome, LaunchOutcome::Refused(_)));
        assert!(!report.proves_hardware_launch());
    }

    #[test]
    fn stage_order_is_monotonic() {
        let ordered = [
            LaunchStage::NotAttempted,
            LaunchStage::PlatformRejected,
            LaunchStage::PackageUnverified,
            LaunchStage::PackageMeasured,
            LaunchStage::HelperSpawned,
            LaunchStage::GuestBooted,
            LaunchStage::FrameAuthenticated,
            LaunchStage::PointerAcknowledged,
            LaunchStage::KeyboardAcknowledged,
            LaunchStage::UnicodeAcknowledged,
            LaunchStage::StoppedAndReaped,
        ];
        for pair in ordered.windows(2) {
            assert!(
                pair[0] < pair[1],
                "{} < {}",
                pair[0].as_str(),
                pair[1].as_str()
            );
        }
    }
}
