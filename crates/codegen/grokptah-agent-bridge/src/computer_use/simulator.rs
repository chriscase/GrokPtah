use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use chrono::Utc;
use parking_lot::Mutex;

use super::types::{
    ActionOutcome, ComputerAction, ComputerBackend, ComputerCapabilities, ComputerCapabilityProof,
    ComputerError, ComputerErrorCode, ComputerObservation, ComputerResult, ComputerTarget,
    ComputerUseLimits, IsolationProofOrigin, ObservationGeometry, PhysicalInputDomain,
    SemanticAction, SemanticElement, Sensitivity, SIMULATOR_BACKGROUND_BACKEND_ID,
    SIMULATOR_FOREGROUND_BACKEND_ID, SIMULATOR_ISOLATED_BACKEND_ID,
};

#[derive(Debug)]
pub struct SimulatorBackend {
    state: Mutex<SimulatorState>,
    proof: ComputerCapabilityProof,
    domain: PhysicalInputDomain,
}

#[derive(Debug)]
struct SimulatorState {
    target: ComputerTarget,
    runs: BTreeMap<String, SimulatorRunState>,
    mutations: u64,
}

#[derive(Debug, Default)]
struct SimulatorRunState {
    sequence: u64,
    name: String,
    submitted: bool,
    content_generation: u64,
    observed_content_generation: u64,
    observed_sequence: u64,
}

impl Default for SimulatorBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SimulatorBackend {
    pub fn new() -> Self {
        Self::foreground_semantic()
    }

    pub fn foreground_semantic() -> Self {
        Self::with_proof(
            ComputerCapabilityProof::ForegroundSemantic {
                backend_id: SIMULATOR_FOREGROUND_BACKEND_ID.into(),
                observe: true,
                semantic_actions: true,
                text_entry: true,
            },
            PhysicalInputDomain::attested("simulator", SIMULATOR_FOREGROUND_BACKEND_ID)
                .expect("simulator foreground domain is attested"),
        )
    }

    pub fn measured_background_safe() -> Self {
        Self::with_proof(
            ComputerCapabilityProof::MeasuredBackgroundSafeSemantic {
                backend_id: SIMULATOR_BACKGROUND_BACKEND_ID.into(),
                observe: true,
                semantic_actions: true,
                text_entry: true,
                measurement_id: uuid::Uuid::new_v4().to_string(),
            },
            PhysicalInputDomain::attested("simulator", SIMULATOR_BACKGROUND_BACKEND_ID)
                .expect("simulator background domain is attested"),
        )
    }

    /// Explicit simulator-only isolated fixture. This proof cannot attest a
    /// native backend as isolated.
    pub fn independently_isolated() -> Self {
        Self::with_proof(
            ComputerCapabilityProof::IndependentlyIsolatedVisualInputDomain {
                backend_id: SIMULATOR_ISOLATED_BACKEND_ID.into(),
                surface_id: uuid::Uuid::new_v4().to_string(),
                incarnation: uuid::Uuid::new_v4().to_string(),
                input_domain_id: uuid::Uuid::new_v4().to_string(),
                origin: IsolationProofOrigin::SimulatorFixture,
                observe: true,
                semantic_actions: true,
                text_entry: true,
                key_chords: true,
                pointer_fallback: true,
            },
            PhysicalInputDomain::attested("simulator", SIMULATOR_ISOLATED_BACKEND_ID)
                .expect("simulator isolated domain is attested"),
        )
    }

    fn with_proof(proof: ComputerCapabilityProof, domain: PhysicalInputDomain) -> Self {
        proof.validate().expect("simulator fixture proof is valid");
        Self {
            state: Mutex::new(SimulatorState {
                target: Self::demo_target(),
                runs: BTreeMap::new(),
                mutations: 0,
            }),
            proof,
            domain,
        }
    }

    pub fn demo_target() -> ComputerTarget {
        ComputerTarget {
            app_id: "com.grokptah.computer-use-simulator".into(),
            window_id: "demo-form".into(),
            generation: 1,
            display_name: "Computer Use Simulator".into(),
            sensitivity: Sensitivity::None,
        }
    }

    pub fn submitted(&self) -> bool {
        self.state.lock().runs.values().any(|run| run.submitted)
    }

    /// Change application content without altering geometry or selected-element
    /// shape. Host freshness ticks are not advanced; `act_if_current` must deny.
    pub fn mutate_content_preserving_shape(&self, run_id: &str) {
        let mut state = self.state.lock();
        let run = state.runs.entry(run_id.into()).or_default();
        run.content_generation = run.content_generation.saturating_add(1);
    }

    pub fn mutation_count(&self) -> u64 {
        self.state.lock().mutations
    }
}

#[async_trait]
impl ComputerBackend for SimulatorBackend {
    fn capabilities(&self) -> ComputerCapabilities {
        ComputerCapabilities::from_proof(self.proof.clone()).expect("simulator proof is valid")
    }

    fn physical_input_domain(&self) -> PhysicalInputDomain {
        self.domain.clone()
    }

    async fn observe(
        &self,
        run_id: &str,
        observation_id: &str,
        target: &ComputerTarget,
        limits: &ComputerUseLimits,
    ) -> ComputerResult<ComputerObservation> {
        let mut state = self.state.lock();
        if target != &state.target {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "simulator target is not authorized",
            ));
        }
        let target = state.target.clone();
        let run = state.runs.entry(run_id.into()).or_default();
        run.sequence = run.sequence.saturating_add(1);
        run.observed_sequence = run.sequence;
        run.observed_content_generation = run.content_generation;
        let name_id = format!("{observation_id}-name");
        let submit_id = format!("{observation_id}-submit");
        let status_id = format!("{observation_id}-status");
        let geometry = ObservationGeometry {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
            scale_factor: 1.0,
        };
        let observation = ComputerObservation {
            observation_id: observation_id.to_string(),
            sequence: run.sequence,
            target,
            captured_at: Utc::now(),
            geometry,
            screenshot: None,
            elements: vec![
                SemanticElement {
                    element_id: name_id,
                    role: "text_field".into(),
                    label: Some("Name".into()),
                    value: (!run.name.is_empty()).then(|| run.name.clone()),
                    bounds: Some(ObservationGeometry {
                        x: 40.0,
                        y: 80.0,
                        width: 400.0,
                        height: 44.0,
                        scale_factor: 1.0,
                    }),
                    enabled: true,
                    focused: false,
                    sensitivity: Sensitivity::None,
                    actions: BTreeSet::from([SemanticAction::SetValue]),
                },
                SemanticElement {
                    element_id: submit_id,
                    role: "button".into(),
                    label: Some("Submit".into()),
                    value: None,
                    bounds: Some(ObservationGeometry {
                        x: 40.0,
                        y: 144.0,
                        width: 120.0,
                        height: 44.0,
                        scale_factor: 1.0,
                    }),
                    enabled: !run.name.is_empty(),
                    focused: false,
                    sensitivity: Sensitivity::None,
                    actions: BTreeSet::from([SemanticAction::Invoke]),
                },
                SemanticElement {
                    element_id: status_id,
                    role: "status".into(),
                    label: Some(if run.submitted {
                        format!("Submitted for {}", run.name)
                    } else {
                        "Not submitted".into()
                    }),
                    value: None,
                    bounds: None,
                    enabled: true,
                    focused: false,
                    sensitivity: Sensitivity::None,
                    actions: BTreeSet::new(),
                },
            ],
            elements_truncated: false,
            sensitivity: Sensitivity::None,
            authority: Default::default(),
        };
        observation.validate(limits)?;
        Ok(observation)
    }

    async fn act(
        &self,
        run_id: &str,
        observation: &ComputerObservation,
        action: &ComputerAction,
    ) -> ComputerResult<ActionOutcome> {
        let mut state = self.state.lock();
        simulator_dispatch(&mut state, &self.proof, run_id, observation, action, false)
    }

    async fn act_if_current(
        &self,
        run_id: &str,
        observation: &ComputerObservation,
        action: &ComputerAction,
    ) -> ComputerResult<ActionOutcome> {
        let mut state = self.state.lock();
        simulator_dispatch(&mut state, &self.proof, run_id, observation, action, true)
    }

    async fn cancel(&self, run_id: &str) -> ComputerResult<()> {
        let _ = run_id;
        Ok(())
    }
}

fn simulator_dispatch(
    state: &mut SimulatorState,
    proof: &ComputerCapabilityProof,
    run_id: &str,
    observation: &ComputerObservation,
    action: &ComputerAction,
    require_current: bool,
) -> ComputerResult<ActionOutcome> {
    if observation.target != state.target {
        return Err(ComputerError::new(
            ComputerErrorCode::ForbiddenTarget,
            "simulator action target is not authorized",
        ));
    }
    let mutated = !matches!(action, ComputerAction::Wait { .. });
    let outcome = {
        let run = state.runs.get_mut(run_id).ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::InvalidState,
                "simulator run has not been observed",
            )
        })?;
        if observation.sequence != run.sequence || observation.sequence != run.observed_sequence {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "simulator action used a stale observation",
            ));
        }
        if require_current && run.content_generation != run.observed_content_generation {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "simulator action is not attested against the current content generation",
            ));
        }
        match action {
            ComputerAction::SetValue { element_id, text }
                if element_id == &format!("{}-name", observation.observation_id) =>
            {
                run.name = text.clone();
                ActionOutcome::bounded("set demo name", Some(true))
            }
            ComputerAction::Invoke { element_id }
                if element_id == &format!("{}-submit", observation.observation_id) =>
            {
                if run.name.is_empty() {
                    return Err(ComputerError::new(
                        ComputerErrorCode::ForbiddenAction,
                        "submit is disabled until a name is entered",
                    ));
                }
                run.submitted = true;
                ActionOutcome::bounded("submitted demo form", Some(true))
            }
            ComputerAction::ActivateTarget if proof.tier().allows_activate_target() => {
                ActionOutcome::bounded("simulator action completed", Some(true))
            }
            ComputerAction::ActivateTarget => {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenAction,
                    "background-safe simulator fixture cannot activate a target",
                ))
            }
            ComputerAction::Wait { .. } => {
                ActionOutcome::bounded("simulator action completed", Some(true))
            }
            ComputerAction::PointerClick { .. } | ComputerAction::KeyChord { .. }
                if proof.is_simulator_only_isolation() =>
            {
                ActionOutcome::bounded("simulator isolated fixture input", Some(true))
            }
            _ => {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenAction,
                    "simulator does not support this action",
                ))
            }
        }
    };
    if mutated {
        state.mutations = state.mutations.saturating_add(1);
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stale_simulator_observation_is_rejected() {
        let backend = SimulatorBackend::new();
        let target = SimulatorBackend::demo_target();
        let first = backend
            .observe(
                "run",
                "observation-1",
                &target,
                &ComputerUseLimits::default(),
            )
            .await
            .unwrap();
        let _second = backend
            .observe(
                "run",
                "observation-2",
                &target,
                &ComputerUseLimits::default(),
            )
            .await
            .unwrap();
        let error = backend
            .act("run", &first, &ComputerAction::ActivateTarget)
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::StaleObservation);
    }

    #[tokio::test]
    async fn observations_are_isolated_between_runs() {
        let backend = SimulatorBackend::new();
        let target = SimulatorBackend::demo_target();
        let first = backend
            .observe(
                "run-one",
                "run-one-observation-1",
                &target,
                &ComputerUseLimits::default(),
            )
            .await
            .unwrap();
        let second = backend
            .observe(
                "run-two",
                "run-two-observation-1",
                &target,
                &ComputerUseLimits::default(),
            )
            .await
            .unwrap();
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 1);
        backend
            .act(
                "run-one",
                &first,
                &ComputerAction::SetValue {
                    element_id: format!("{}-name", first.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .unwrap();
        let second_again = backend
            .observe(
                "run-two",
                "run-two-observation-2",
                &target,
                &ComputerUseLimits::default(),
            )
            .await
            .unwrap();
        assert!(second_again.elements[0].value.is_none());
    }

    #[tokio::test]
    async fn capability_fixtures_are_explicit_and_isolated_is_simulator_only() {
        let background = SimulatorBackend::measured_background_safe();
        assert_eq!(
            background.capabilities().tier,
            crate::computer_use::ComputerCapabilityTier::MeasuredBackgroundSafeSemantic
        );
        assert!(!background.capabilities().pointer_fallback);
        assert!(!background.capabilities().key_chords);
        let target = SimulatorBackend::demo_target();
        let observation = background
            .observe(
                "bg",
                "bg-observation",
                &target,
                &ComputerUseLimits::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            background
                .act("bg", &observation, &ComputerAction::ActivateTarget)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::ForbiddenAction
        );

        let isolated = SimulatorBackend::independently_isolated();
        assert_eq!(
            isolated.capabilities().tier,
            crate::computer_use::ComputerCapabilityTier::IndependentlyIsolatedVisualInputDomain
        );
        assert_eq!(
            isolated.capabilities().proof.isolation_origin(),
            Some(IsolationProofOrigin::SimulatorFixture)
        );
        assert!(isolated.capabilities().proof.is_simulator_only_isolation());
        assert_ne!(
            isolated.capabilities().backend_id,
            crate::computer_use::MACOS_NATIVE_BACKEND_ID
        );
        let isolated_obs = isolated
            .observe(
                "iso",
                "iso-observation",
                &target,
                &ComputerUseLimits::default(),
            )
            .await
            .unwrap();
        isolated
            .act(
                "iso",
                &isolated_obs,
                &ComputerAction::PointerClick {
                    x: 10.0,
                    y: 10.0,
                    button: crate::computer_use::PointerButton::Primary,
                },
            )
            .await
            .unwrap();
        isolated
            .act(
                "iso",
                &isolated_obs,
                &ComputerAction::KeyChord {
                    keys: vec![crate::computer_use::ComputerKey::Enter],
                },
            )
            .await
            .unwrap();
        assert!(!SimulatorBackend::new()
            .capabilities()
            .proof
            .isolated_input_is_dispatchable());
    }
}
