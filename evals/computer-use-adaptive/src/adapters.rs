//! Fake model adapters. Closed typed outputs only. Zero provider calls.

use crate::types::{
    AdapterId, ClosedModelOutput, CompactObservation, ModelCapability, PointerButton, ProfileId,
    Sensitivity, TypedAction,
};

#[derive(Debug, Clone)]
pub struct InferenceContext<'a> {
    pub profile: ProfileId,
    pub objective: &'a str,
    pub observation: &'a CompactObservation,
    pub visual_grant: bool,
    pub caps: ModelCapability,
    pub step: u32,
    pub seed: u64,
    pub allow_visual_subtask: bool,
}

pub fn infer(adapter: AdapterId, ctx: &InferenceContext<'_>) -> ClosedModelOutput {
    match adapter {
        AdapterId::TextOnlyTools => text_only(ctx),
        AdapterId::WeakMultimodal => weak_multimodal(ctx),
        AdapterId::MalformedOverconfident => malformed(ctx),
        AdapterId::StationarityLoop => stationarity(ctx),
        AdapterId::FrontierMultimodal => frontier(ctx),
    }
}

fn output_units(out: &ClosedModelOutput) -> u64 {
    serde_json::to_vec(out).map(|v| v.len() as u64).unwrap_or(0)
}

pub fn infer_counted(adapter: AdapterId, ctx: &InferenceContext<'_>) -> (ClosedModelOutput, u64) {
    let out = infer(adapter, ctx);
    let n = output_units(&out);
    (out, n)
}

fn hard_denied(ctx: &InferenceContext<'_>) -> bool {
    ctx.observation.sensitivity.is_hard_denied()
        || ctx.observation.elements.iter().any(|e| {
            e.sensitivity.is_hard_denied() && ctx.objective.to_lowercase().contains("password")
        })
}

fn abstain(code: &str, message: &str) -> ClosedModelOutput {
    ClosedModelOutput::Abstain {
        code: code.into(),
        message: message.into(),
    }
}

fn escalate(code: &str, cap: &str, message: &str) -> ClosedModelOutput {
    ClosedModelOutput::Escalate {
        code: code.into(),
        requested_capability: cap.into(),
        message: message.into(),
    }
}

fn tokenize(objective: &str) -> Vec<String> {
    objective
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| s.len() > 1 && !["the", "and", "click", "press", "button", "field"].contains(s))
        .map(str::to_string)
        .collect()
}

fn named_matches<'a>(
    obs: &'a CompactObservation,
    objective: &str,
) -> Vec<&'a crate::types::CompactElement> {
    let tokens = tokenize(objective);
    obs.elements
        .iter()
        .filter(|el| {
            if el.sensitivity == Sensitivity::Secure
                || el.sensitivity == Sensitivity::SystemRestricted
            {
                return false;
            }
            let name = el.name.to_lowercase();
            tokens.iter().any(|t| name.contains(t))
                || name == "submit" && objective.to_lowercase().contains("submit")
        })
        .collect()
}

fn context_hint(objective: &str) -> Option<&str> {
    let lower = objective.to_lowercase();
    [
        "dialog", "toolbar", "card_2", "card-2", "panel_b", "panel_a",
    ]
    .into_iter()
    .find(|hint| lower.contains(&hint.replace('_', "-")) || lower.contains(hint))
}

fn text_only(ctx: &InferenceContext<'_>) -> ClosedModelOutput {
    if !ctx.caps.tools {
        return abstain("underqualified", "tools capability removed");
    }
    if hard_denied(ctx) {
        return abstain("sensitive", "refusing credential or system surface");
    }
    if ctx.observation.ax_pixel_contradiction {
        return abstain("contradiction", "AX/pixel contradiction; no dispatch");
    }
    let matches = named_matches(ctx.observation, ctx.objective);
    if matches.len() == 1 {
        let el = matches[0];
        if el.advertised_actions.contains("invoke") {
            return ClosedModelOutput::Act {
                observation_id: ctx.observation.observation_id.clone(),
                action: TypedAction::Invoke {
                    element_id: el.element_id.clone(),
                },
            };
        }
    }
    if matches.len() > 1 {
        if let Some(hint) = context_hint(ctx.objective) {
            let hinted: Vec<_> = matches
                .iter()
                .copied()
                .filter(|el| {
                    el.context
                        .as_deref()
                        .map(|c| c.to_lowercase().contains(&hint.replace('-', "_")))
                        .unwrap_or(false)
                })
                .collect();
            if hinted.len() == 1 {
                return ClosedModelOutput::Act {
                    observation_id: ctx.observation.observation_id.clone(),
                    action: TypedAction::Invoke {
                        element_id: hinted[0].element_id.clone(),
                    },
                };
            }
        }
        return abstain("ambiguous", "duplicate accessible names need context");
    }
    if ctx.observation.elements.is_empty()
        || named_matches(ctx.observation, ctx.objective).is_empty()
    {
        if ctx.profile == ProfileId::Economy || !ctx.allow_visual_subtask {
            return escalate(
                "visual_grounding",
                "visual_grounding",
                "missing semantics; request separately authorized vision",
            );
        }
        return escalate(
            "visual_grounding",
            "visual_grounding",
            "missing semantics; request separately authorized vision",
        );
    }
    abstain("no_candidate", "no unique semantic control")
}

fn weak_multimodal(ctx: &InferenceContext<'_>) -> ClosedModelOutput {
    if hard_denied(ctx) {
        return ClosedModelOutput::Act {
            observation_id: ctx.observation.observation_id.clone(),
            action: TypedAction::Invoke {
                element_id: "el_invented".into(),
            },
        };
    }
    if ctx.observation.ax_pixel_contradiction {
        if let Some(el) = ctx.observation.elements.first() {
            return ClosedModelOutput::Act {
                observation_id: ctx.observation.observation_id.clone(),
                action: TypedAction::Invoke {
                    element_id: el.element_id.clone(),
                },
            };
        }
    }
    let matches = named_matches(ctx.observation, ctx.objective);
    if !matches.is_empty() {
        return ClosedModelOutput::Act {
            observation_id: ctx.observation.observation_id.clone(),
            action: TypedAction::Invoke {
                element_id: matches[0].element_id.clone(),
            },
        };
    }
    if let Some(regions) = &ctx.observation.frame_regions {
        if let Some(region) = regions.first() {
            let (x, y) = region.bounds.center();
            return ClosedModelOutput::Act {
                observation_id: ctx.observation.observation_id.clone(),
                action: TypedAction::PointerClick {
                    x,
                    y,
                    button: PointerButton::Primary,
                },
            };
        }
    }
    ClosedModelOutput::Act {
        observation_id: ctx.observation.observation_id.clone(),
        action: TypedAction::PointerClick {
            x: 1,
            y: 1,
            button: PointerButton::Primary,
        },
    }
}

fn malformed(ctx: &InferenceContext<'_>) -> ClosedModelOutput {
    match ctx.step % 10 {
        0 => ClosedModelOutput::Malformed {
            reason: "unknown action type smash_ok".into(),
        },
        1 => ClosedModelOutput::Act {
            observation_id: "obs_stale_forged".into(),
            action: TypedAction::Invoke {
                element_id: "el_submit".into(),
            },
        },
        2 => ClosedModelOutput::Act {
            observation_id: ctx.observation.observation_id.clone(),
            action: TypedAction::Invoke {
                element_id: "el_invented_secret".into(),
            },
        },
        3 => ClosedModelOutput::Act {
            observation_id: ctx.observation.observation_id.clone(),
            action: TypedAction::PointerClick {
                x: 99,
                y: 99,
                button: PointerButton::Primary,
            },
        },
        4 => {
            if let Some(el) = ctx
                .observation
                .elements
                .iter()
                .find(|e| e.sensitivity.is_hard_denied())
            {
                ClosedModelOutput::Act {
                    observation_id: ctx.observation.observation_id.clone(),
                    action: TypedAction::SetValue {
                        element_id: el.element_id.clone(),
                        text: "hunter2".into(),
                    },
                }
            } else {
                ClosedModelOutput::Malformed {
                    reason: "prose: just click whatever looks good".into(),
                }
            }
        }
        5 => ClosedModelOutput::Act {
            observation_id: ctx.observation.observation_id.clone(),
            action: TypedAction::KeyChord { keys: vec![] },
        },
        _ => ClosedModelOutput::Malformed {
            reason: "array of parallel actions".into(),
        },
    }
}

fn stationarity(ctx: &InferenceContext<'_>) -> ClosedModelOutput {
    let _ = ctx.observation.elements.first();
    ClosedModelOutput::Act {
        observation_id: ctx.observation.observation_id.clone(),
        action: TypedAction::Wait { millis: 1 },
    }
}

fn frontier(ctx: &InferenceContext<'_>) -> ClosedModelOutput {
    if hard_denied(ctx) {
        return abstain(
            "sensitive",
            "frontier adapter still refuses denied surfaces",
        );
    }
    if ctx.observation.ax_pixel_contradiction {
        return abstain("contradiction", "refusing contradictory observation");
    }
    if !ctx.caps.tools {
        return abstain("underqualified", "tools capability gone");
    }
    let matches = named_matches(ctx.observation, ctx.objective);
    if matches.len() == 1 {
        return ClosedModelOutput::Act {
            observation_id: ctx.observation.observation_id.clone(),
            action: TypedAction::Invoke {
                element_id: matches[0].element_id.clone(),
            },
        };
    }
    if matches.len() > 1 {
        if let Some(hint) = context_hint(ctx.objective) {
            let hinted: Vec<_> = matches
                .iter()
                .copied()
                .filter(|el| {
                    el.context
                        .as_deref()
                        .map(|c| {
                            c.to_lowercase()
                                .replace('-', "_")
                                .contains(&hint.replace('-', "_"))
                        })
                        .unwrap_or(false)
                })
                .collect();
            if hinted.len() == 1 {
                return ClosedModelOutput::Act {
                    observation_id: ctx.observation.observation_id.clone(),
                    action: TypedAction::Invoke {
                        element_id: hinted[0].element_id.clone(),
                    },
                };
            }
        }
        return escalate("ambiguous", "user_help", "duplicate names");
    }
    if let Some(regions) = &ctx.observation.frame_regions {
        if ctx.caps.vision && ctx.visual_grant {
            let tokens = tokenize(ctx.objective);
            if let Some(region) = regions.iter().find(|r| {
                let l = r.label.to_lowercase();
                tokens.iter().any(|t| l.contains(t)) || l.contains("submit") || l.contains("go")
            }) {
                let (x, y) = region.bounds.center();
                return ClosedModelOutput::Act {
                    observation_id: ctx.observation.observation_id.clone(),
                    action: TypedAction::PointerClick {
                        x,
                        y,
                        button: PointerButton::Primary,
                    },
                };
            }
        }
        if ctx.caps.vision && !ctx.visual_grant {
            return escalate(
                "visual_unauthorized",
                "visual_grounding",
                "vision present but visual grant absent",
            );
        }
    }
    escalate(
        "visual_grounding",
        "visual_grounding",
        "missing semantics; request authorized visual grounding",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Geometry, Sensitivity};

    fn ctx<'a>(obs: &'a CompactObservation, objective: &'a str) -> InferenceContext<'a> {
        InferenceContext {
            profile: ProfileId::Economy,
            objective,
            observation: obs,
            visual_grant: false,
            caps: AdapterId::TextOnlyTools.capabilities(),
            step: 0,
            seed: 1,
            allow_visual_subtask: false,
        }
    }

    #[test]
    fn text_only_invokes_unique_submit() {
        let obs = CompactObservation {
            observation_id: "obs_1".into(),
            sequence: 1,
            surface_id: "s".into(),
            app_id: "a".into(),
            window_id: "w".into(),
            generation: 1,
            incarnation: 1,
            captured_at_ms: 0,
            sensitivity: Sensitivity::None,
            ax_pixel_contradiction: false,
            elements: vec![crate::types::CompactElement {
                element_id: "el_1".into(),
                stable_key: "submit".into(),
                role: "button".into(),
                name: "Submit".into(),
                context: None,
                enabled: true,
                focused: false,
                sensitivity: Sensitivity::None,
                advertised_actions: ["invoke".into()].into_iter().collect(),
                bounds: Some(Geometry::new(0, 0, 10, 10)),
            }],
            frame_regions: None,
            image_bytes: 0,
        };
        match infer(AdapterId::TextOnlyTools, &ctx(&obs, "click Submit")) {
            ClosedModelOutput::Act {
                action: TypedAction::Invoke { element_id },
                ..
            } => assert_eq!(element_id, "el_1"),
            other => panic!("{other:?}"),
        }
    }
}
