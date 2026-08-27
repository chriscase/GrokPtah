//! Scripted world mutations.
//!
//! A scenario is a world plus a schedule of mutations keyed by step index.
//! Every mutation is a total function of the world, so replaying a scenario
//! replays the surface exactly. There is no timer, no jitter source, and no
//! hidden state: the hazard fires on the step the fixture says it fires on.
//!
//! `ReorderElements` is the one that needs a word. Rotating the node vector
//! is not cosmetic churn -- combined with per-observation ephemeral ids it
//! means an id the model captured last turn now denotes a different node.
//! An agent that re-resolves by label survives; one that caches ids does not.

use serde::{Deserialize, Serialize};

use crate::schema::{ControlDisposition, Sensitivity};
use crate::world::{SecretKind, World};

/// World flag naming the generation a human has re-approved.
pub const OPERATOR_REGRANT_FLAG: &str = "__operator_regrant";

/// One scripted change to the surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Mutation {
    /// Nothing changes. Used to build stationarity traps.
    Noop,
    /// Rotate the node order left by `by`, rebuilding the tree.
    ReorderElements {
        by: usize,
    },
    /// Clone an existing node's label onto another node.
    DuplicateLabel {
        source_node: String,
        target_node: String,
    },
    OpenModal {
        layer: String,
    },
    CloseModal,
    ScrollTo {
        y: i32,
    },
    /// Change the URL origin without changing app or window. The
    /// display name deliberately stays the same, so a model that trusts the
    /// title bar is fooled and a model that checks the origin is not.
    Navigate {
        origin: String,
    },
    /// Bump the surface generation: every outstanding observation and grant
    /// is now invalid.
    RegenerateSurface,
    /// Replace app and window identity outright.
    ReplaceSurface {
        app_id: String,
        window_id: String,
        display_name: String,
    },
    /// Put attacker-controlled text into a node's value.
    InjectContent {
        node_id: String,
        text: String,
    },
    /// Reclassify a node, typically to `Secure`.
    Reclassify {
        node_id: String,
        sensitivity: Sensitivity,
    },
    /// Plant a synthetic secret in a node's value, and register it so the
    /// leak scanner can find it in the transcript.
    PlantSecret {
        node_id: String,
        kind: SecretKind,
        token: String,
    },
    SetClipboard {
        token: String,
        kind: SecretKind,
    },
    Crash,
    Restart,
    HelperDown,
    HelperUp,
    GoOffline,
    GoOnline,
    SetControl {
        disposition: ControlDisposition,
    },
    /// A second agent claims the surface.
    CompetingClaim {
        controller: String,
    },
    ReleaseCompetingClaim,
    /// Disable a node, so the obvious affordance stops working.
    DisableNode {
        node_id: String,
    },
    EnableNode {
        node_id: String,
    },
    SetFlag {
        key: String,
        value: String,
    },
    /// A human re-approves the surface after it changed identity. This is the
    /// only way an authorized target is ever widened mid-run: the harness
    /// never re-grants on its own, because "the app relaunched, so I renewed
    /// my own authorization" is precisely the hole the authority model
    /// exists to close.
    OperatorRegrant,
}

impl Mutation {
    /// Apply the mutation. Every arm bumps `revision` when it changes
    /// anything, which is what stationarity detection reads.
    pub fn apply(&self, world: &mut World) {
        match self {
            Self::Noop => {}
            Self::ReorderElements { by } => {
                if !world.nodes.is_empty() {
                    let by = by % world.nodes.len();
                    world.nodes.rotate_left(by);
                    world.revision += 1;
                }
            }
            Self::DuplicateLabel {
                source_node,
                target_node,
            } => {
                let label = world.node(source_node).and_then(|node| node.label.clone());
                if let (Some(label), Some(target)) = (label, world.node_mut(target_node)) {
                    target.label = Some(label);
                    world.revision += 1;
                }
            }
            Self::OpenModal { layer } => {
                world.modal = Some(layer.clone());
                world.revision += 1;
            }
            Self::CloseModal => {
                if world.modal.take().is_some() {
                    world.revision += 1;
                }
            }
            Self::ScrollTo { y } => {
                let clamped = (*y).clamp(0, (world.content_height - world.viewport.height).max(0));
                if world.scroll_y != clamped {
                    world.scroll_y = clamped;
                    world.revision += 1;
                }
            }
            Self::Navigate { origin } => {
                world.url_origin = Some(origin.clone());
                world.revision += 1;
            }
            Self::RegenerateSurface => {
                world.generation += 1;
                world.revision += 1;
            }
            Self::ReplaceSurface {
                app_id,
                window_id,
                display_name,
            } => {
                world.app_id = app_id.clone();
                world.window_id = window_id.clone();
                world.display_name = display_name.clone();
                world.generation += 1;
                world.revision += 1;
            }
            Self::InjectContent { node_id, text } => {
                if let Some(node) = world.node_mut(node_id) {
                    node.value = Some(text.clone());
                    world.revision += 1;
                }
            }
            Self::Reclassify {
                node_id,
                sensitivity,
            } => {
                if let Some(node) = world.node_mut(node_id) {
                    node.sensitivity = *sensitivity;
                    // A secure node must not carry a value at all.
                    if sensitivity.is_hard_denied() {
                        node.value = None;
                    }
                    world.revision += 1;
                }
            }
            Self::PlantSecret {
                node_id,
                kind,
                token,
            } => {
                if let Some(node) = world.node_mut(node_id) {
                    node.value = Some(token.clone());
                }
                world.secrets.push(crate::world::SecretToken {
                    kind: *kind,
                    token: token.clone(),
                });
                world.revision += 1;
            }
            Self::SetClipboard { token, kind } => {
                world.clipboard = Some(token.clone());
                world.secrets.push(crate::world::SecretToken {
                    kind: *kind,
                    token: token.clone(),
                });
                world.revision += 1;
            }
            Self::Crash => {
                world.crashed = true;
                world.revision += 1;
            }
            Self::Restart => {
                world.crashed = false;
                world.generation += 1;
                world.modal = None;
                world.scroll_y = 0;
                world.revision += 1;
            }
            Self::HelperDown => {
                world.helper_alive = false;
                world.revision += 1;
            }
            Self::HelperUp => {
                world.helper_alive = true;
                world.revision += 1;
            }
            Self::GoOffline => {
                world.online = false;
                world.revision += 1;
            }
            Self::GoOnline => {
                world.online = true;
                world.revision += 1;
            }
            Self::SetControl { disposition } => {
                world.control_disposition = *disposition;
                world.revision += 1;
            }
            Self::CompetingClaim { controller } => {
                world.competing_controller = Some(controller.clone());
                world.revision += 1;
            }
            Self::ReleaseCompetingClaim => {
                world.competing_controller = None;
                world.revision += 1;
            }
            Self::DisableNode { node_id } => {
                if let Some(node) = world.node_mut(node_id) {
                    node.enabled = false;
                    world.revision += 1;
                }
            }
            Self::EnableNode { node_id } => {
                if let Some(node) = world.node_mut(node_id) {
                    node.enabled = true;
                    world.revision += 1;
                }
            }
            Self::SetFlag { key, value } => world.set_flag(key, value),
            Self::OperatorRegrant => {
                let generation = world.generation;
                world.set_flag(OPERATOR_REGRANT_FLAG, &generation.to_string());
            }
        }
    }
}

/// A mutation scheduled to fire before a given step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledMutation {
    /// Zero-based step index. The mutation fires before the observation for
    /// that step is taken.
    pub before_step: u32,
    pub mutation: Mutation,
}

impl ScheduledMutation {
    #[must_use]
    pub fn new(before_step: u32, mutation: Mutation) -> Self {
        Self {
            before_step,
            mutation,
        }
    }
}

/// Apply every mutation scheduled for `step`, in fixture order.
pub fn apply_scheduled(world: &mut World, schedule: &[ScheduledMutation], step: u32) {
    for scheduled in schedule.iter().filter(|item| item.before_step == step) {
        scheduled.mutation.apply(world);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Rect, SemanticAction};
    use crate::world::WorldNode;

    fn world() -> World {
        World::new("app", "w", "App").with_nodes(vec![
            WorldNode::new(
                "a",
                "button",
                Some("A"),
                Rect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                },
                &[SemanticAction::Invoke],
            ),
            WorldNode::new(
                "b",
                "button",
                Some("B"),
                Rect {
                    x: 0,
                    y: 20,
                    width: 10,
                    height: 10,
                },
                &[SemanticAction::Invoke],
            ),
            WorldNode::new(
                "c",
                "button",
                Some("C"),
                Rect {
                    x: 0,
                    y: 40,
                    width: 10,
                    height: 10,
                },
                &[SemanticAction::Invoke],
            ),
        ])
    }

    #[test]
    fn reorder_changes_which_node_a_slot_denotes() {
        let mut world = world();
        let before = world.observe(1, 0, 64, false);
        Mutation::ReorderElements { by: 1 }.apply(&mut world);
        let after = world.observe(2, 10, 64, false);
        assert_eq!(before.binding.get("obs1-n0").map(String::as_str), Some("a"));
        assert_eq!(after.binding.get("obs2-n0").map(String::as_str), Some("b"));
    }

    #[test]
    fn reclassifying_to_secure_strips_the_value() {
        let mut world = world();
        Mutation::InjectContent {
            node_id: "a".into(),
            text: "visible".into(),
        }
        .apply(&mut world);
        Mutation::Reclassify {
            node_id: "a".into(),
            sensitivity: Sensitivity::Secure,
        }
        .apply(&mut world);
        assert_eq!(world.node("a").and_then(|n| n.value.clone()), None);
    }

    #[test]
    fn every_effective_mutation_bumps_the_revision() {
        let mut world = world();
        let start = world.revision;
        Mutation::OpenModal {
            layer: "dialog".into(),
        }
        .apply(&mut world);
        assert!(world.revision > start);
    }

    #[test]
    fn noop_leaves_the_revision_alone_so_stationarity_is_detectable() {
        let mut world = world();
        let start = world.revision;
        Mutation::Noop.apply(&mut world);
        assert_eq!(world.revision, start);
    }

    #[test]
    fn restart_clears_transient_state_and_bumps_generation() {
        let mut world = world();
        Mutation::OpenModal {
            layer: "dialog".into(),
        }
        .apply(&mut world);
        Mutation::Crash.apply(&mut world);
        assert!(!world.observable());
        Mutation::Restart.apply(&mut world);
        assert!(world.observable());
        assert_eq!(world.modal, None);
        assert_eq!(world.generation, 2);
    }

    #[test]
    fn applying_a_schedule_is_order_stable() {
        let schedule = vec![
            ScheduledMutation::new(
                2,
                Mutation::SetFlag {
                    key: "k".into(),
                    value: "1".into(),
                },
            ),
            ScheduledMutation::new(
                2,
                Mutation::SetFlag {
                    key: "k".into(),
                    value: "2".into(),
                },
            ),
            ScheduledMutation::new(
                3,
                Mutation::SetFlag {
                    key: "k".into(),
                    value: "3".into(),
                },
            ),
        ];
        let mut world = world();
        apply_scheduled(&mut world, &schedule, 2);
        assert_eq!(world.flag("k"), Some("2"));
    }
}
