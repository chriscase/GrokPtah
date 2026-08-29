//! Deterministic host simulator: clocks, IDs, grants, leases, crash cuts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::policy::{authorize, DenyCode, PolicyView};
use crate::profile::ProfileBudget;
use crate::types::{
    ActionClass, CompactElement, CompactObservation, CrashCut, EvalError, EvalResult, FrameRegion,
    Geometry, ModelCapability, ProfileId, Sensitivity, TimeoutClass, TypedAction,
    STATIONARITY_WINDOW,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct EffectSpec {
    #[serde(rename = "type")]
    pub kind: String,
    pub flag: Option<String>,
    pub key: Option<String>,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ElementSpec {
    pub stable_key: String,
    pub role: String,
    pub name: String,
    pub context: Option<String>,
    pub value: Option<String>,
    pub enabled: bool,
    pub focused: bool,
    pub sensitivity: Sensitivity,
    pub advertised_actions: Vec<String>,
    pub bounds: Geometry,
    pub effect: Option<EffectSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct SurfaceSpec {
    pub surface_id: String,
    pub conflict_domain: String,
    pub isolated: bool,
    pub app_id: String,
    pub window_id: String,
    pub generation: u64,
    pub display_name: String,
    pub geometry: Geometry,
    pub sensitivity: Sensitivity,
    pub elements: Vec<ElementSpec>,
    pub frame_regions: Vec<FrameRegion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct GrantSpec {
    pub grant_id: String,
    pub action_classes: Vec<ActionClass>,
    pub expires_at_ms: u64,
    pub remaining_uses: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct VisualGrant {
    pub granted: bool,
    pub grant_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    pub agent_id: String,
    pub work_attempt_id: String,
    pub lease_id: String,
    pub surface_id: String,
    pub lease_state: crate::types::LeaseState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct WorldSpec {
    pub run_id: String,
    pub surfaces: Vec<SurfaceSpec>,
    pub grant: Option<GrantSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual_grant: Option<VisualGrant>,
    pub agents: Vec<AgentSpec>,
    pub ax_pixel_contradiction: bool,
    pub consequential: bool,
    pub success_flag: String,
}

impl WorldSpec {
    pub fn visual_granted(&self) -> bool {
        self.visual_grant.as_ref().is_some_and(|g| g.granted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum EventKind {
    Takeover {},
    Cancel {},
    TimeoutBeforeSend {},
    TimeoutAfterSend {},
    TimeoutAfterInput {},
    CrashBeforeSend {},
    CrashAfterSend {},
    CrashAfterInput {},
    Restart {},
    DowngradeVision {},
    DowngradeTools {},
    MoveTarget {},
    ResizeTarget {},
    RestartTarget {},
    AdvanceOtherAgent {},
    GrantVisual {},
    ExpireGrant {},
    SecondAgentSameDomain {},
    SecondAgentIsolated {},
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventPhase {
    StepStart,
    AfterObserve,
    BeforeDispatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ScheduledEvent {
    pub at_step: u32,
    pub phase: EventPhase,
    pub event: EventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct TraceEvent {
    pub step: u32,
    pub clock_ms: u64,
    pub kind: TraceKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceKind {
    Agent,
    Cancel,
    Contention,
    Crash,
    Deny,
    Dispatch,
    Downgrade,
    Grant,
    Observe,
    Overlap,
    Restart,
    Takeover,
    Target,
    Timeout,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct PhysicalRecord {
    pub dispatch_id: String,
    pub permitted: bool,
    pub agent_id: String,
    pub surface_id: String,
    pub conflict_domain: String,
    pub clock_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ObservationRecord {
    pub observation_id: String,
    pub encoded_bytes: u64,
    pub image_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SurfaceState {
    spec: SurfaceSpec,
    incarnation: u64,
    elements: Vec<ElementSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LeaseRec {
    agent_id: String,
    surface_id: String,
    conflict_domain: String,
    granted: bool,
    dispatching: bool,
    incarnation: u64,
    revoked: bool,
}

#[derive(Debug, Clone)]
pub struct Host {
    pub clock: u64,
    pub step: u32,
    pub seed: u64,
    pub profile: ProfileId,
    pub caps: ModelCapability,
    pub run_id: String,
    surfaces: BTreeMap<String, SurfaceState>,
    grant: Option<GrantSpec>,
    visual_grant: bool,
    visual_grant_id: Option<String>,
    leases: BTreeMap<String, LeaseRec>,
    observations: BTreeMap<String, CompactObservation>,
    current_obs: BTreeMap<String, String>,
    element_owner: BTreeMap<String, (String, String)>,
    pub flags: BTreeMap<String, bool>,
    pub physical: Vec<PhysicalRecord>,
    send_ledger: Vec<String>,
    pub takeover: bool,
    pub cancelled: bool,
    pub timeout: Option<TimeoutClass>,
    pub crash: Option<CrashCut>,
    pub restarts: u32,
    pub uncertain: bool,
    obs_seq: u64,
    dispatch_seq: u64,
    pub invalid: u64,
    pub stale: u64,
    pub unauthorized: u64,
    pub abstentions: u64,
    pub escalations: u64,
    pub postcondition_failures: u64,
    pub observation_bytes: u64,
    pub image_bytes: u64,
    pub model_input_units: u64,
    pub model_output_units: u64,
    action_fps: Vec<String>,
    obs_fps: Vec<String>,
    pub trace: Vec<TraceEvent>,
    pub ax_pixel_contradiction: bool,
    pub consequential: bool,
    pub success_flag: String,
    pub primary_agent: String,
    pub primary_surface: String,
    pub primary_lease: String,
    script: Vec<ScheduledEvent>,
    domain_busy: BTreeMap<String, String>,
}

impl Host {
    pub fn new(
        world: WorldSpec,
        profile: ProfileId,
        caps: ModelCapability,
        seed: u64,
        script: Vec<ScheduledEvent>,
    ) -> Self {
        let mut surfaces = BTreeMap::new();
        for spec in &world.surfaces {
            surfaces.insert(
                spec.surface_id.clone(),
                SurfaceState {
                    incarnation: 1,
                    elements: spec.elements.clone(),
                    spec: spec.clone(),
                },
            );
        }
        let mut leases = BTreeMap::new();
        for agent in &world.agents {
            let domain = world
                .surfaces
                .iter()
                .find(|s| s.surface_id == agent.surface_id)
                .map(|s| s.conflict_domain.clone())
                .unwrap_or_else(|| "domain_default".into());
            leases.insert(
                agent.lease_id.clone(),
                LeaseRec {
                    agent_id: agent.agent_id.clone(),
                    surface_id: agent.surface_id.clone(),
                    conflict_domain: domain,
                    granted: matches!(
                        agent.lease_state,
                        crate::types::LeaseState::Granted | crate::types::LeaseState::Dispatching
                    ),
                    dispatching: false,
                    incarnation: 1,
                    revoked: false,
                },
            );
        }
        let primary = world.agents.first();
        let visual_granted = world.visual_granted();
        let visual_grant_id = world
            .visual_grant
            .as_ref()
            .filter(|g| g.granted)
            .map(|g| g.grant_id.clone());
        Self {
            clock: 0,
            step: 0,
            seed,
            profile,
            caps,
            run_id: world.run_id,
            surfaces,
            grant: world.grant,
            visual_grant: visual_granted,
            visual_grant_id,
            leases,
            observations: BTreeMap::new(),
            current_obs: BTreeMap::new(),
            element_owner: BTreeMap::new(),
            flags: BTreeMap::new(),
            physical: Vec::new(),
            send_ledger: Vec::new(),
            takeover: false,
            cancelled: false,
            timeout: None,
            crash: None,
            restarts: 0,
            uncertain: false,
            obs_seq: 0,
            dispatch_seq: 0,
            invalid: 0,
            stale: 0,
            unauthorized: 0,
            abstentions: 0,
            escalations: 0,
            postcondition_failures: 0,
            observation_bytes: 0,
            image_bytes: 0,
            model_input_units: 0,
            model_output_units: 0,
            action_fps: Vec::new(),
            obs_fps: Vec::new(),
            trace: Vec::new(),
            ax_pixel_contradiction: world.ax_pixel_contradiction,
            consequential: world.consequential,
            success_flag: world.success_flag,
            primary_agent: primary
                .map(|a| a.agent_id.clone())
                .unwrap_or_else(|| "agent_a".into()),
            primary_surface: primary
                .map(|a| a.surface_id.clone())
                .unwrap_or_else(|| "surface_a".into()),
            primary_lease: primary
                .map(|a| a.lease_id.clone())
                .unwrap_or_else(|| "lease_a".into()),
            script,
            domain_busy: BTreeMap::new(),
        }
    }

    pub fn tick(&mut self, ms: u64) {
        self.clock = self.clock.saturating_add(ms);
    }

    fn log(&mut self, kind: &str, detail: impl Into<String>) {
        let kind = match kind {
            "agent" => TraceKind::Agent,
            "cancel" => TraceKind::Cancel,
            "contention" => TraceKind::Contention,
            "crash" => TraceKind::Crash,
            "deny" => TraceKind::Deny,
            "dispatch" => TraceKind::Dispatch,
            "downgrade" => TraceKind::Downgrade,
            "grant" => TraceKind::Grant,
            "observe" => TraceKind::Observe,
            "overlap" => TraceKind::Overlap,
            "restart" => TraceKind::Restart,
            "takeover" => TraceKind::Takeover,
            "target" => TraceKind::Target,
            "timeout" => TraceKind::Timeout,
            "uncertain" => TraceKind::Uncertain,
            other => panic!("untyped trace kind: {other}"),
        };
        self.trace.push(TraceEvent {
            step: self.step,
            clock_ms: self.clock,
            kind,
            detail: {
                let mut d = detail.into();
                d.truncate(256);
                d
            },
        });
    }

    pub fn observation_records(&self) -> Vec<ObservationRecord> {
        self.observations
            .values()
            .map(|observation| ObservationRecord {
                observation_id: observation.observation_id.clone(),
                encoded_bytes: serde_json::to_vec(observation)
                    .map(|bytes| bytes.len() as u64)
                    .unwrap_or(0),
                image_bytes: observation.image_bytes,
            })
            .collect()
    }

    pub fn physical_records(&self) -> Vec<PhysicalRecord> {
        self.physical.clone()
    }

    /// Test/evaluator fault injection: simulates a backend that mutates despite
    /// a denied kernel decision. The evidence oracle must always detect this.
    pub fn inject_unauthorized_backend_mutation(&mut self) {
        self.dispatch_seq += 1;
        let dispatch_id = format!("disp_injected_{}", self.dispatch_seq);
        self.physical.push(PhysicalRecord {
            dispatch_id: dispatch_id.clone(),
            permitted: false,
            agent_id: self.primary_agent.clone(),
            surface_id: self.primary_surface.clone(),
            conflict_domain: self
                .surfaces
                .get(&self.primary_surface)
                .map(|surface| surface.spec.conflict_domain.clone())
                .unwrap_or_default(),
            clock_ms: self.clock,
        });
        self.unauthorized = self.unauthorized.saturating_add(1);
        self.log("dispatch", format!("{dispatch_id};permitted=false"));
    }

    pub fn apply_script(&mut self, phase: EventPhase) {
        let events: Vec<EventKind> = self
            .script
            .iter()
            .filter(|e| e.at_step == self.step && e.phase == phase)
            .map(|e| e.event)
            .collect();
        for event in events {
            self.apply_event(event);
        }
    }

    pub fn apply_event(&mut self, event: EventKind) {
        match event {
            EventKind::Takeover {} => {
                self.takeover = true;
                for lease in self.leases.values_mut() {
                    lease.revoked = true;
                    lease.granted = false;
                    lease.dispatching = false;
                }
                self.domain_busy.clear();
                self.log("takeover", "operator takeover is absorbing");
            }
            EventKind::Cancel {} => {
                self.cancelled = true;
                self.log("cancel", "run cancelled");
            }
            EventKind::TimeoutBeforeSend {} => {
                self.timeout = Some(TimeoutClass::DefinitelyBeforeSend);
                self.log("timeout", "definitely_before_send");
            }
            EventKind::TimeoutAfterSend {} => {
                self.timeout = Some(TimeoutClass::UncertainAfterSend);
                self.log("timeout", "uncertain_after_send");
            }
            EventKind::TimeoutAfterInput {} => {
                self.timeout = Some(TimeoutClass::UncertainAfterInput);
                self.log("timeout", "uncertain_after_input");
            }
            EventKind::CrashBeforeSend {} => {
                self.crash = Some(CrashCut::BeforeSend);
                self.log("crash", "before_send");
            }
            EventKind::CrashAfterSend {} => {
                self.crash = Some(CrashCut::AfterSend);
                self.log("crash", "after_send");
            }
            EventKind::CrashAfterInput {} => {
                self.crash = Some(CrashCut::AfterInput);
                self.log("crash", "after_input");
            }
            EventKind::Restart {} => self.restart(),
            EventKind::DowngradeVision {} => {
                self.caps.vision = false;
                self.log("downgrade", "vision removed; higher tier not retained");
            }
            EventKind::DowngradeTools {} => {
                self.caps.tools = false;
                self.log("downgrade", "tools removed");
            }
            EventKind::MoveTarget {} => {
                if let Some(s) = self.surfaces.get_mut(&self.primary_surface) {
                    s.spec.geometry.x += 40;
                    s.spec.geometry.y += 12;
                }
                self.invalidate_obs(&self.primary_surface.clone());
                self.log("target", "moved");
            }
            EventKind::ResizeTarget {} => {
                if let Some(s) = self.surfaces.get_mut(&self.primary_surface) {
                    s.spec.geometry.width = s.spec.geometry.width.saturating_add(80);
                }
                self.invalidate_obs(&self.primary_surface.clone());
                self.log("target", "resized");
            }
            EventKind::RestartTarget {} => {
                if let Some(s) = self.surfaces.get_mut(&self.primary_surface) {
                    s.spec.generation += 1;
                    s.incarnation += 1;
                }
                self.invalidate_obs(&self.primary_surface.clone());
                self.log("target", "restarted generation");
            }
            EventKind::AdvanceOtherAgent {} => {
                if let Some(s) = self.surfaces.get_mut(&self.primary_surface) {
                    s.spec.generation += 1;
                }
                self.invalidate_obs(&self.primary_surface.clone());
                self.log("contention", "other agent advanced shared surface");
            }
            EventKind::GrantVisual {} => {
                self.visual_grant = true;
                if self.visual_grant_id.is_none() {
                    self.visual_grant_id = Some("vgrant_eval".into());
                }
                self.log("grant", "visual grounding authorized separately");
            }
            EventKind::ExpireGrant {} => {
                if let Some(grant) = self.grant.as_mut() {
                    grant.expires_at_ms = self.clock;
                }
                self.log("grant", "expired");
            }
            EventKind::SecondAgentSameDomain {} => {
                self.ensure_second_agent(false);
            }
            EventKind::SecondAgentIsolated {} => {
                self.ensure_second_agent(true);
            }
        }
    }

    fn ensure_second_agent(&mut self, isolated: bool) {
        let domain = if isolated {
            "domain_isolated_b".into()
        } else {
            self.surfaces
                .get(&self.primary_surface)
                .map(|s| s.spec.conflict_domain.clone())
                .unwrap_or_else(|| "domain_fg".into())
        };
        if isolated {
            let mut spec = self
                .surfaces
                .get(&self.primary_surface)
                .map(|s| s.spec.clone())
                .unwrap_or_else(|| SurfaceSpec {
                    surface_id: "surface_b".into(),
                    conflict_domain: domain.clone(),
                    isolated: true,
                    app_id: "app.demo.b".into(),
                    window_id: "win_b".into(),
                    generation: 1,
                    display_name: "DemoB".into(),
                    geometry: Geometry::new(0, 0, 200, 100),
                    sensitivity: Sensitivity::None,
                    elements: Vec::new(),
                    frame_regions: Vec::new(),
                });
            spec.surface_id = "surface_b".into();
            spec.conflict_domain = domain.clone();
            spec.isolated = true;
            spec.window_id = "win_b".into();
            spec.app_id = "app.demo.b".into();
            if spec.elements.is_empty() {
                spec.elements.push(ElementSpec {
                    stable_key: "submit_b".into(),
                    role: "button".into(),
                    name: "Submit".into(),
                    context: Some("panel_b".into()),
                    value: None,
                    enabled: true,
                    focused: false,
                    sensitivity: Sensitivity::None,
                    advertised_actions: vec!["invoke".into()],
                    bounds: Geometry::new(8, 8, 48, 16),
                    effect: Some(EffectSpec {
                        kind: "set_flag".into(),
                        flag: Some("submitted_b".into()),
                        key: None,
                        value: None,
                    }),
                });
            }
            self.surfaces.insert(
                "surface_b".into(),
                SurfaceState {
                    incarnation: 1,
                    elements: spec.elements.clone(),
                    spec,
                },
            );
        }
        self.leases.insert(
            "lease_b".into(),
            LeaseRec {
                agent_id: "agent_b".into(),
                surface_id: if isolated {
                    "surface_b".into()
                } else {
                    self.primary_surface.clone()
                },
                conflict_domain: domain,
                granted: true,
                dispatching: false,
                incarnation: 1,
                revoked: false,
            },
        );
        self.log(
            "agent",
            if isolated {
                "second agent isolated domain"
            } else {
                "second agent same domain"
            },
        );
    }

    fn invalidate_obs(&mut self, surface_id: &str) {
        self.current_obs.remove(surface_id);
    }

    pub fn restart(&mut self) {
        self.restarts += 1;
        for surface in self.surfaces.values_mut() {
            surface.incarnation += 1;
        }
        for lease in self.leases.values_mut() {
            lease.revoked = true;
            lease.granted = false;
            lease.dispatching = false;
        }
        self.domain_busy.clear();
        self.current_obs.clear();
        if matches!(
            self.timeout,
            Some(TimeoutClass::UncertainAfterSend | TimeoutClass::UncertainAfterInput)
        ) || matches!(self.crash, Some(CrashCut::AfterSend | CrashCut::AfterInput))
        {
            self.uncertain = true;
        }
        self.crash = None;
        self.log(
            "restart",
            format!(
                "incarnation bumped; live leases revoked; restart {}",
                self.restarts
            ),
        );
        // Old incarnation is never auto-resumed. A fresh grant/lease would be
        // required for further dispatch; the eval does not mint one.
    }

    pub fn observe(&mut self, surface_id: &str) -> EvalResult<CompactObservation> {
        self.obs_seq += 1;
        self.tick(5);
        let budget = ProfileBudget::for_profile(self.profile);
        let surface = self
            .surfaces
            .get(surface_id)
            .cloned()
            .ok_or_else(|| EvalError::Host(format!("unknown surface {surface_id}")))?;
        let mut elements = Vec::new();
        let mut owner = Vec::new();
        for (i, el) in surface.elements.iter().enumerate() {
            if i >= budget.max_observation_elements {
                break;
            }
            if el.sensitivity.is_hard_denied() && el.value.is_some() {
                continue;
            }
            let element_id = format!("el_{}_{}", self.obs_seq, el.stable_key);
            owner.push((
                element_id.clone(),
                (surface_id.to_string(), el.stable_key.clone()),
            ));
            let include =
                !el.sensitivity.is_hard_denied() || el.sensitivity == Sensitivity::Potential;
            if !include && el.sensitivity.is_hard_denied() {
                // Sensitive nodes are present as markers without values.
            }
            elements.push(CompactElement {
                element_id,
                stable_key: el.stable_key.clone(),
                role: el.role.clone(),
                name: el.name.clone(),
                context: el.context.clone(),
                enabled: el.enabled && !el.sensitivity.is_hard_denied(),
                focused: el.focused,
                sensitivity: el.sensitivity,
                advertised_actions: if el.sensitivity.is_hard_denied() {
                    Default::default()
                } else {
                    el.advertised_actions.iter().cloned().collect()
                },
                bounds: if budget.include_element_bounds {
                    Some(el.bounds)
                } else {
                    None
                },
            });
        }
        let (frame_regions, image_bytes) = if budget.allow_screenshot
            && self.caps.vision
            && !surface.spec.frame_regions.is_empty()
        {
            let bytes = serde_json::to_vec(&surface.spec.frame_regions)
                .map(|v| v.len() as u64)
                .unwrap_or(0)
                .min(budget.max_image_bytes);
            (Some(surface.spec.frame_regions.clone()), bytes)
        } else {
            (None, 0)
        };
        let obs = CompactObservation {
            observation_id: format!(
                "obs_{}_{}_{}_{}",
                surface_id, self.obs_seq, surface.spec.generation, surface.incarnation
            ),
            sequence: self.obs_seq,
            surface_id: surface_id.to_string(),
            app_id: surface.spec.app_id.clone(),
            window_id: surface.spec.window_id.clone(),
            generation: surface.spec.generation,
            incarnation: surface.incarnation,
            captured_at_ms: self.clock,
            sensitivity: surface.spec.sensitivity,
            ax_pixel_contradiction: self.ax_pixel_contradiction,
            elements,
            frame_regions,
            image_bytes,
        };
        let bytes = serde_json::to_vec(&obs)
            .map(|v| v.len() as u64)
            .unwrap_or(0);
        self.observation_bytes = self.observation_bytes.saturating_add(bytes);
        self.image_bytes = self.image_bytes.saturating_add(image_bytes);
        self.model_input_units = self.model_input_units.saturating_add(bytes);
        self.obs_fps.push(obs.observation_id.clone());
        self.current_obs
            .insert(surface_id.to_string(), obs.observation_id.clone());
        self.observations
            .insert(obs.observation_id.clone(), obs.clone());
        for (eid, own) in owner {
            self.element_owner.insert(eid, own);
        }
        self.log("observe", obs.observation_id.clone());
        Ok(obs)
    }

    pub fn policy_view(&self, surface_id: &str, lease_id: &str) -> PolicyView {
        let surface = self.surfaces.get(surface_id);
        let lease = self.leases.get(lease_id);
        let domain = lease.map(|l| l.conflict_domain.as_str()).unwrap_or("");
        let busy = self
            .domain_busy
            .get(domain)
            .map(|owner| owner != lease_id)
            .unwrap_or(false);
        PolicyView {
            profile: self.profile,
            caps: self.caps,
            takeover: self.takeover,
            cancelled: self.cancelled,
            timeout_before_send: matches!(self.timeout, Some(TimeoutClass::DefinitelyBeforeSend))
                || matches!(self.crash, Some(CrashCut::BeforeSend)),
            grant_present: self.grant.is_some(),
            grant_expired: self
                .grant
                .as_ref()
                .is_some_and(|grant| self.clock >= grant.expires_at_ms),
            grant_exhausted: self.grant.as_ref().and_then(|grant| grant.remaining_uses) == Some(0),
            grant_classes: self
                .grant
                .as_ref()
                .map(|grant| grant.action_classes.clone())
                .unwrap_or_default(),
            visual_granted: self.visual_grant,
            lease_granted: lease.map(|l| l.granted && !l.revoked).unwrap_or(false),
            domain_busy: busy,
            current_observation_id: self.current_obs.get(surface_id).cloned(),
            surface_generation: surface.map(|s| s.spec.generation).unwrap_or(0),
            surface_incarnation: surface.map(|s| s.incarnation).unwrap_or(0),
            surface_sensitivity: surface
                .map(|s| s.spec.sensitivity)
                .unwrap_or(Sensitivity::None),
        }
    }

    pub fn try_dispatch(
        &mut self,
        surface_id: &str,
        lease_id: &str,
        observation_id: &str,
        action: &TypedAction,
    ) -> Result<String, DenyCode> {
        let view = self.policy_view(surface_id, lease_id);
        let Some(obs) = self.observations.get(observation_id).cloned() else {
            self.invalid += 1;
            self.stale += 1;
            return Err(DenyCode::MissingObservation);
        };
        if let Err(code) = authorize(&view, &obs, observation_id, action) {
            if code.is_stale() {
                self.stale += 1;
            } else {
                self.invalid += 1;
            }
            self.log("deny", code.as_str());
            return Err(code);
        }
        if matches!(self.crash, Some(CrashCut::BeforeSend))
            || matches!(self.timeout, Some(TimeoutClass::DefinitelyBeforeSend))
        {
            self.log("deny", "timeout_or_crash_before_send");
            return Err(DenyCode::TimeoutBeforeSend);
        }

        self.dispatch_seq += 1;
        let dispatch_id = format!("disp_{}_{}_{}", lease_id, self.dispatch_seq, self.obs_seq);
        self.send_ledger.push(dispatch_id.clone());
        self.tick(3);

        if matches!(self.crash, Some(CrashCut::AfterSend))
            || matches!(self.timeout, Some(TimeoutClass::UncertainAfterSend))
        {
            self.uncertain = true;
            if let Some(lease) = self.leases.get_mut(lease_id) {
                lease.dispatching = false;
                lease.revoked = true;
                lease.granted = false;
            }
            self.log("uncertain", "after_send; no physical input; no replay");
            return Err(DenyCode::TimeoutBeforeSend);
        }

        if self.takeover {
            self.unauthorized = self.unauthorized.saturating_add(0);
            self.log("deny", "takeover_won_dispatch_race");
            return Err(DenyCode::Takeover);
        }

        let domain = self
            .leases
            .get(lease_id)
            .map(|l| l.conflict_domain.clone())
            .unwrap_or_default();
        if let Some(owner) = self.domain_busy.get(&domain) {
            if owner != lease_id {
                self.invalid += 1;
                return Err(DenyCode::LeaseContention);
            }
        }
        self.domain_busy
            .insert(domain.clone(), lease_id.to_string());
        if let Some(lease) = self.leases.get_mut(lease_id) {
            lease.dispatching = true;
        }

        let agent_id = self
            .leases
            .get(lease_id)
            .map(|l| l.agent_id.clone())
            .unwrap_or_default();
        self.physical.push(PhysicalRecord {
            dispatch_id: dispatch_id.clone(),
            permitted: true,
            agent_id,
            surface_id: surface_id.to_string(),
            conflict_domain: domain.clone(),
            clock_ms: self.clock,
        });
        self.apply_action(surface_id, action);
        self.action_fps.push(self.stable_action_fp(action));
        self.invalidate_obs(surface_id);
        self.tick(2);

        if matches!(self.crash, Some(CrashCut::AfterInput))
            || matches!(self.timeout, Some(TimeoutClass::UncertainAfterInput))
        {
            self.uncertain = true;
            self.log("uncertain", "after_input; receipt untrusted; no replay");
        }

        if let Some(lease) = self.leases.get_mut(lease_id) {
            lease.dispatching = false;
        }
        self.domain_busy.remove(&domain);
        if let Some(grant) = self.grant.as_mut() {
            if let Some(uses) = grant.remaining_uses {
                grant.remaining_uses = Some(uses.saturating_sub(1));
            }
        }
        self.log("dispatch", dispatch_id.clone());
        Ok(dispatch_id)
    }

    /// Concurrent isolated dispatch at the same virtual clock, used by family 12.
    pub fn try_dispatch_pair(
        &mut self,
        a: (&str, &str, &str, &TypedAction),
        b: (&str, &str, &str, &TypedAction),
    ) -> (Result<String, DenyCode>, Result<String, DenyCode>) {
        let domain_a = self
            .leases
            .get(a.1)
            .map(|l| l.conflict_domain.clone())
            .unwrap_or_default();
        let domain_b = self
            .leases
            .get(b.1)
            .map(|l| l.conflict_domain.clone())
            .unwrap_or_default();
        if domain_a == domain_b {
            let first = self.try_dispatch(a.0, a.1, a.2, a.3);
            let second = self.try_dispatch(b.0, b.1, b.2, b.3);
            return (first, second);
        }
        let first = self.try_dispatch(a.0, a.1, a.2, a.3);
        let second = self.try_dispatch(b.0, b.1, b.2, b.3);
        if first.is_ok() && second.is_ok() {
            if let (Some(pa), Some(pb)) = (self.physical.iter().rev().nth(1), self.physical.last())
            {
                if pa.clock_ms == pb.clock_ms || pa.conflict_domain != pb.conflict_domain {
                    self.log(
                        "overlap",
                        "isolated domains dispatched without same-domain overlap",
                    );
                }
            }
        }
        (first, second)
    }

    fn apply_action(&mut self, surface_id: &str, action: &TypedAction) {
        let Some(eid) = action.referenced_element() else {
            if let TypedAction::PointerClick { x, y, .. } = action {
                let (x, y) = (*x, *y);
                let hit = self.surfaces.get(surface_id).and_then(|s| {
                    s.spec
                        .frame_regions
                        .iter()
                        .find(|r| r.bounds.contains(x, y))
                        .map(|r| r.label.clone())
                });
                if let Some(label) = hit {
                    if label.to_lowercase().contains("submit")
                        || label.to_lowercase().contains("go")
                        || label.to_lowercase() == self.success_flag
                    {
                        self.flags.insert(self.success_flag.clone(), true);
                    }
                }
            }
            return;
        };
        let Some((sid, key)) = self.element_owner.get(eid).cloned() else {
            return;
        };
        if sid != surface_id {
            return;
        }
        let effect = self
            .surfaces
            .get(&sid)
            .and_then(|s| s.elements.iter().find(|e| e.stable_key == key))
            .and_then(|e| e.effect.clone());
        if let Some(effect) = effect {
            match effect.kind.as_str() {
                "set_flag" => {
                    if let Some(flag) = effect.flag {
                        self.flags.insert(flag, true);
                    }
                }
                "set_value" => {
                    if let Some(k) = effect.key {
                        self.flags.insert(k, true);
                    }
                }
                "bump_generation" => {
                    if let Some(s) = self.surfaces.get_mut(&sid) {
                        s.spec.generation += 1;
                    }
                }
                _ => {}
            }
        }
    }

    fn stable_action_fp(&self, action: &TypedAction) -> String {
        match action {
            TypedAction::Invoke { element_id } => {
                let key = self
                    .element_owner
                    .get(element_id)
                    .map(|(_, k)| k.as_str())
                    .unwrap_or(element_id);
                format!("invoke:{key}")
            }
            TypedAction::Select { element_id } => {
                let key = self
                    .element_owner
                    .get(element_id)
                    .map(|(_, k)| k.as_str())
                    .unwrap_or(element_id);
                format!("select:{key}")
            }
            TypedAction::SetValue { element_id, .. } => {
                let key = self
                    .element_owner
                    .get(element_id)
                    .map(|(_, k)| k.as_str())
                    .unwrap_or(element_id);
                format!("set_value:{key}")
            }
            TypedAction::Scroll {
                element_id,
                delta_x,
                delta_y,
            } => format!(
                "scroll:{}:{delta_x}:{delta_y}",
                element_id
                    .as_ref()
                    .and_then(|id| self.element_owner.get(id).map(|(_, k)| k.as_str()))
                    .unwrap_or("-")
            ),
            TypedAction::Wait { millis } => format!("wait:{millis}"),
            TypedAction::PointerClick { x, y, button } => {
                format!("pointer:{x}:{y}:{button:?}")
            }
            TypedAction::ActivateTarget => "activate".into(),
            TypedAction::KeyChord { keys } => format!("chord:{keys:?}"),
        }
    }

    pub fn stationarity(&self) -> bool {
        if self.action_fps.len() < STATIONARITY_WINDOW {
            return false;
        }
        let n = self.action_fps.len();
        let last = &self.action_fps[n - STATIONARITY_WINDOW..];
        last.iter().all(|a| a == &last[0])
    }

    pub fn task_success(&self) -> bool {
        self.flags.get(&self.success_flag).copied().unwrap_or(false)
            && self.unauthorized == 0
            && !self.uncertain
    }

    pub fn recovery_converged(&self) -> Option<bool> {
        if self.restarts < 2 {
            return None;
        }
        let orphan_grant = self.leases.values().any(|l| l.granted && !l.revoked);
        let dup = {
            let mut ids: Vec<_> = self
                .physical
                .iter()
                .map(|p| p.dispatch_id.as_str())
                .collect();
            ids.sort();
            ids.windows(2).any(|w| w[0] == w[1])
        };
        Some(!orphan_grant && !dup)
    }

    pub fn grant_id(&self) -> Option<&str> {
        self.grant.as_ref().map(|grant| grant.grant_id.as_str())
    }

    pub fn visual_grant_id(&self) -> Option<&str> {
        self.visual_grant_id.as_deref()
    }

    pub fn lease_ids(&self) -> Vec<String> {
        self.leases.keys().cloned().collect()
    }

    pub fn observation_ids(&self) -> Vec<String> {
        self.observations.keys().cloned().collect()
    }

    pub fn dispatch_ids(&self) -> Vec<String> {
        self.physical
            .iter()
            .map(|p| p.dispatch_id.clone())
            .collect()
    }

    pub fn same_domain_physical_concurrency(&self) -> u64 {
        let mut max_c = 0_u64;
        let mut by_clock: BTreeMap<(u64, String), u64> = BTreeMap::new();
        for p in &self.physical {
            *by_clock
                .entry((p.clock_ms, p.conflict_domain.clone()))
                .or_insert(0) += 1;
        }
        for c in by_clock.values() {
            max_c = max_c.max(*c);
        }
        max_c
    }

    pub fn record_output_units(&mut self, n: u64) {
        self.model_output_units = self.model_output_units.saturating_add(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AdapterId;

    fn demo_world() -> WorldSpec {
        WorldSpec {
            run_id: "run_demo".into(),
            surfaces: vec![SurfaceSpec {
                surface_id: "surface_a".into(),
                conflict_domain: "domain_fg".into(),
                isolated: false,
                app_id: "app.demo".into(),
                window_id: "win_a".into(),
                generation: 1,
                display_name: "Demo".into(),
                geometry: Geometry::new(0, 0, 400, 200),
                sensitivity: Sensitivity::None,
                elements: vec![ElementSpec {
                    stable_key: "submit".into(),
                    role: "button".into(),
                    name: "Submit".into(),
                    context: None,
                    value: None,
                    enabled: true,
                    focused: false,
                    sensitivity: Sensitivity::None,
                    advertised_actions: vec!["invoke".into()],
                    bounds: Geometry::new(10, 10, 60, 20),
                    effect: Some(EffectSpec {
                        kind: "set_flag".into(),
                        flag: Some("submitted".into()),
                        key: None,
                        value: None,
                    }),
                }],
                frame_regions: vec![],
            }],
            grant: Some(GrantSpec {
                grant_id: "grant_a".into(),
                action_classes: vec![ActionClass::Semantic, ActionClass::TextEntry],
                expires_at_ms: 1_000_000,
                remaining_uses: Some(8),
            }),
            visual_grant: None,
            agents: vec![AgentSpec {
                agent_id: "agent_a".into(),
                work_attempt_id: "wa_a".into(),
                lease_id: "lease_a".into(),
                surface_id: "surface_a".into(),
                lease_state: crate::types::LeaseState::Granted,
            }],
            ax_pixel_contradiction: false,
            consequential: false,
            success_flag: "submitted".into(),
        }
    }

    #[test]
    fn unique_semantic_dispatch_sets_flag() {
        let mut host = Host::new(
            demo_world(),
            ProfileId::Economy,
            AdapterId::TextOnlyTools.capabilities(),
            1,
            vec![],
        );
        let obs = host.observe("surface_a").unwrap();
        let el = obs.elements[0].element_id.clone();
        let id = host
            .try_dispatch(
                "surface_a",
                "lease_a",
                &obs.observation_id,
                &TypedAction::Invoke { element_id: el },
            )
            .unwrap();
        assert!(id.starts_with("disp_"));
        assert!(host.task_success());
        assert_eq!(host.unauthorized, 0);
    }

    #[test]
    fn stale_observation_does_not_dispatch() {
        let mut host = Host::new(
            demo_world(),
            ProfileId::Economy,
            AdapterId::TextOnlyTools.capabilities(),
            1,
            vec![],
        );
        let first = host.observe("surface_a").unwrap();
        let el = first.elements[0].element_id.clone();
        let _ = host.observe("surface_a").unwrap();
        let err = host
            .try_dispatch(
                "surface_a",
                "lease_a",
                &first.observation_id,
                &TypedAction::Invoke { element_id: el },
            )
            .unwrap_err();
        assert_eq!(err, DenyCode::StaleObservation);
        assert!(host.physical.is_empty());
    }
}
