use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use chrono::Utc;
use parking_lot::Mutex;

use super::types::{
    ActionOutcome, ComputerAction, ComputerBackend, ComputerCapabilities, ComputerError,
    ComputerErrorCode, ComputerObservation, ComputerResult, ComputerTarget, ComputerUseLimits,
    ObservationGeometry, SemanticAction, SemanticElement, Sensitivity,
};

#[derive(Debug)]
pub struct SimulatorBackend {
    state: Mutex<SimulatorState>,
}

#[derive(Debug)]
struct SimulatorState {
    target: ComputerTarget,
    runs: BTreeMap<String, SimulatorRunState>,
}

#[derive(Debug, Default)]
struct SimulatorRunState {
    sequence: u64,
    name: String,
    submitted: bool,
}

impl Default for SimulatorBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SimulatorBackend {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(SimulatorState {
                target: Self::demo_target(),
                runs: BTreeMap::new(),
            }),
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
}

#[async_trait]
impl ComputerBackend for SimulatorBackend {
    fn capabilities(&self) -> ComputerCapabilities {
        ComputerCapabilities {
            backend_id: "deterministic_simulator".into(),
            observe: true,
            semantic_actions: true,
            text_entry: true,
            key_chords: false,
            pointer_fallback: false,
        }
    }

    async fn observe(
        &self,
        run_id: &str,
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
        let observation_id = format!("sim-observation-{}", run.sequence);
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
            observation_id,
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
        if observation.target != state.target {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "simulator action target is not authorized",
            ));
        }
        let run = state.runs.get_mut(run_id).ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::InvalidState,
                "simulator run has not been observed",
            )
        })?;
        if observation.sequence != run.sequence {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "simulator action used a stale observation",
            ));
        }
        match action {
            ComputerAction::SetValue { element_id, text }
                if element_id == &format!("{}-name", observation.observation_id) =>
            {
                run.name = text.clone();
                Ok(ActionOutcome::bounded("set demo name", Some(true)))
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
                Ok(ActionOutcome::bounded("submitted demo form", Some(true)))
            }
            ComputerAction::ActivateTarget | ComputerAction::Wait { .. } => Ok(
                ActionOutcome::bounded("simulator action completed", Some(true)),
            ),
            _ => Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "simulator does not support this action",
            )),
        }
    }

    async fn cancel(&self, run_id: &str) -> ComputerResult<()> {
        let _ = run_id;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stale_simulator_observation_is_rejected() {
        let backend = SimulatorBackend::new();
        let target = SimulatorBackend::demo_target();
        let first = backend
            .observe("run", &target, &ComputerUseLimits::default())
            .await
            .unwrap();
        let _second = backend
            .observe("run", &target, &ComputerUseLimits::default())
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
            .observe("run-one", &target, &ComputerUseLimits::default())
            .await
            .unwrap();
        let second = backend
            .observe("run-two", &target, &ComputerUseLimits::default())
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
            .observe("run-two", &target, &ComputerUseLimits::default())
            .await
            .unwrap();
        assert!(second_again.elements[0].value.is_none());
    }
}
