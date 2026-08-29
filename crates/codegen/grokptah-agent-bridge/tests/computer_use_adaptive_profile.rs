//! Deterministic offline campaign for the adaptive Computer Use profiles (#435).
//!
//! Nothing here calls a provider, opens a socket, or touches a screen. Every
//! "model" is a pure function from a rendered observation to the bytes a real
//! provider would have returned, so the campaign is reproducible from its
//! fixtures alone.
//!
//! # What this is trying to falsify
//!
//! The claim under test is the one issue #435 makes and the one a cheaper
//! profile is most likely to break: **Economy is an efficiency policy, never a
//! reduced-safety mode.** Concretely:
//!
//! 1. *Safety is profile-independent.* The universal seal
//!    (`accept_model_proposal`, #473) takes no profile argument at all, and
//!    profile narrowing runs strictly after it. Anything the seal refuses is
//!    refused in every profile.
//! 2. *Rendering is bounded, deterministic, and redaction-safe.* No profile
//!    renders a hard-denied element, screenshot bytes, an evidence token, or a
//!    content hash, and the same frame renders byte-identically every time.
//! 3. *Economy costs strictly less.* On the same frame, the cheap view is a
//!    subset of the rich one and sends fewer bytes.

use std::collections::BTreeSet;

use chrono::Utc;
use grokptah_agent_bridge::computer_use::{ObservationGeometry, SemanticElement, Sensitivity};
use grokptah_agent_bridge::{
    render_computer_observation, AdaptiveProfile, ComputerObservation, ComputerTarget,
    SemanticAction,
};

/// Repeats per adapter × profile cell. The adapters are deterministic, so a
/// repeat proves *stability*, not a distribution: a cell that answered
/// differently on repeat 4 than on repeat 0 would be a defect.
const REPEATS: usize = 5;

/// Named frame builder. A `type` alias rather than an inline tuple type: the
/// inline form trips `clippy::type_complexity` under the repo's `-D warnings`
/// gate, which CI runs on macOS where the lib compiles and the test target is
/// actually linted.
type NamedFrame = (&'static str, fn() -> ComputerObservation);

/// Named model adapter, same reasoning.
type NamedAdapter = (&'static str, fn(&serde_json::Value, usize) -> String);

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn target() -> ComputerTarget {
    ComputerTarget {
        app_id: "com.grokptah.demo".into(),
        window_id: "main".into(),
        generation: 1,
        display_name: "Demo".into(),
        sensitivity: Sensitivity::None,
    }
}

fn element(
    id: &str,
    label: &str,
    action: SemanticAction,
    enabled: bool,
    focused: bool,
) -> SemanticElement {
    SemanticElement {
        element_id: id.into(),
        role: "button".into(),
        label: Some(label.into()),
        value: None,
        bounds: Some(ObservationGeometry {
            x: 4.0,
            y: 8.0,
            width: 80.0,
            height: 24.0,
            scale_factor: 2.0,
        }),
        enabled,
        focused,
        sensitivity: Sensitivity::None,
        actions: BTreeSet::from([action]),
    }
}

fn observation(elements: Vec<SemanticElement>) -> ComputerObservation {
    ComputerObservation {
        observation_id: "obs-1".into(),
        sequence: 1,
        target: target(),
        captured_at: Utc::now(),
        geometry: ObservationGeometry {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
            scale_factor: 2.0,
        },
        screenshot: None,
        elements,
        elements_truncated: false,
        sensitivity: Sensitivity::None,
    }
}

/// One unique enabled control. The easy case every profile should manage.
fn unique_semantic_frame() -> ComputerObservation {
    observation(vec![
        element("save", "Save", SemanticAction::Invoke, true, true),
        element("cancel", "Cancel", SemanticAction::Invoke, true, false),
    ])
}

/// Two controls with the identical accessible name. Semantics alone cannot
/// disambiguate them, which is what the ambiguity signal is for.
fn duplicate_label_frame() -> ComputerObservation {
    observation(vec![
        element("open-a", "Open", SemanticAction::Invoke, true, false),
        element("open-b", "Open", SemanticAction::Invoke, true, false),
    ])
}

/// A surface whose only control is disabled: nothing is actionable.
fn missing_semantics_frame() -> ComputerObservation {
    observation(vec![element(
        "greyed",
        "Save",
        SemanticAction::Invoke,
        false,
        false,
    )])
}

/// A frame carrying a hard-denied credential field alongside a safe control.
fn sensitive_frame() -> ComputerObservation {
    let mut secret = element(
        "password",
        "Passphrase",
        SemanticAction::SetValue,
        true,
        true,
    );
    secret.sensitivity = Sensitivity::Secure;
    secret.label = Some("CREDENTIAL-NEEDLE".into());
    observation(vec![
        secret,
        element("ok", "OK", SemanticAction::Invoke, true, false),
    ])
}

// ---------------------------------------------------------------------------
// Model adapters
// ---------------------------------------------------------------------------

/// A small text-oriented gateway model: picks the first rendered actionable
/// element and proposes the action that element advertises.
fn text_only_gateway(rendered: &serde_json::Value, _repeat: usize) -> String {
    let observation_id = rendered["observation_id"].as_str().unwrap_or_default();
    let Some(element) = rendered["elements"]
        .as_array()
        .and_then(|elements| elements.iter().find(|element| element["enabled"] == true))
    else {
        return serde_json::json!({
            "observation_id": observation_id,
            "action_type": "complete",
            "summary": "Nothing actionable is visible"
        })
        .to_string();
    };
    serde_json::json!({
        "observation_id": observation_id,
        "action_type": "invoke",
        "element_id": element["element_id"],
        "summary": "Activate the first advertised control"
    })
    .to_string()
}

/// A small multimodal model reaching for pixels it was never given: it
/// proposes coordinates, which are not in the closed action grammar at all.
fn weak_multimodal(rendered: &serde_json::Value, _repeat: usize) -> String {
    serde_json::json!({
        "observation_id": rendered["observation_id"],
        "action_type": "pointer_click",
        "delta_x": 120,
        "delta_y": 240,
        "summary": "Click where the button looks like it is"
    })
    .to_string()
}

/// An overconfident model returning malformed, over-scoped, and injected
/// payloads. It varies by repeat so the campaign covers several shapes.
fn malformed_overconfident(rendered: &serde_json::Value, repeat: usize) -> String {
    let observation_id = rendered["observation_id"].clone();
    match repeat % 6 {
        0 => "I am confident the Save button is the right choice.".into(),
        1 => serde_json::json!({
            "observation_id": observation_id,
            "action_type": "invoke",
            "element_id": "save",
            "summary": "Save it",
            "shell": "rm -rf /"
        })
        .to_string(),
        2 => serde_json::json!({
            "observation_id": observation_id,
            "action_type": "invoke",
            "element_id": "admin-panel",
            "summary": "Definitely the right control"
        })
        .to_string(),
        3 => serde_json::json!({
            "observation_id": observation_id,
            "action_type": "complete",
            "element_id": "save",
            "summary": "Done, and also click Save"
        })
        .to_string(),
        4 => serde_json::json!({
            "observation_id": "obs-from-last-minute",
            "action_type": "invoke",
            "element_id": "save",
            "summary": "100% certain this is current"
        })
        .to_string(),
        // Duplicate JSON keys: `serde_json` resolves last-key-wins, which would
        // let one payload mean two different things to a validator and an
        // applier. #473's strict reader rejects it.
        _ => format!(
            r#"{{"observation_id":{observation_id},"action_type":"invoke","element_id":"save","element_id":"admin-panel","summary":"twice"}}"#
        ),
    }
}

/// A model stuck in a loop: the identical proposal every time, forever.
fn stationarity_loop(rendered: &serde_json::Value, _repeat: usize) -> String {
    serde_json::json!({
        "observation_id": rendered["observation_id"],
        "action_type": "invoke",
        "element_id": "save",
        "summary": "Press Save"
    })
    .to_string()
}

/// A frontier-class model: well-formed, in-scope, bound to the exact frame.
fn frontier(rendered: &serde_json::Value, _repeat: usize) -> String {
    let element = rendered["elements"]
        .as_array()
        .and_then(|elements| elements.first())
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "observation_id": rendered["observation_id"],
        "action_type": "invoke",
        "element_id": element["element_id"],
        "summary": "Activate the focused control named by the objective"
    })
    .to_string()
}

const ADAPTERS: [NamedAdapter; 5] = [
    ("text_only_gateway", text_only_gateway),
    ("weak_multimodal", weak_multimodal),
    ("malformed_overconfident", malformed_overconfident),
    ("stationarity_loop", stationarity_loop),
    ("frontier", frontier),
];

const FRAMES: [NamedFrame; 4] = [
    ("unique_semantic", unique_semantic_frame),
    ("duplicate_labels", duplicate_label_frame),
    ("missing_semantics", missing_semantics_frame),
    ("sensitive_surface", sensitive_frame),
];

// ---------------------------------------------------------------------------
// Campaign
// ---------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq, Eq)]
struct CampaignCounts {
    episodes: usize,
    /// Frames whose bounded view offered nothing to act on.
    unactionable_views: usize,
    /// Frames whose bounded view contained duplicate accessible names among
    /// actionable candidates. Non-zero is the ambiguity signal's producer.
    ambiguous_views: usize,
    /// Rendered payloads that leaked a hard-denied label, an evidence token, a
    /// content hash, or image bytes. Any non-zero value fails the campaign.
    redaction_leaks: usize,
    /// Cells where the accounted byte count disagreed with the payload.
    accounting_mismatches: usize,
    /// Cells where a cheaper profile rendered more than a richer one.
    monotonicity_violations: usize,
}

fn run_campaign() -> CampaignCounts {
    let mut counts = CampaignCounts::default();
    for (_frame_name, frame) in FRAMES {
        let observation = frame();
        for (adapter_name, respond) in ADAPTERS {
            for repeat in 0..REPEATS {
                let mut previous_bytes = 0u64;
                for profile in AdaptiveProfile::ALL {
                    counts.episodes += 1;
                    let (payload, rendered) = render_computer_observation(&observation, profile);
                    let text = payload.to_string();

                    for needle in [
                        "CREDENTIAL-NEEDLE",
                        "asset_id",
                        "content_sha256",
                        "byte_len",
                        "media_type",
                    ] {
                        if text.contains(needle) {
                            counts.redaction_leaks += 1;
                        }
                    }
                    if rendered.bytes != text.len() as u64 {
                        counts.accounting_mismatches += 1;
                    }
                    if rendered.bytes < previous_bytes {
                        counts.monotonicity_violations += 1;
                    }
                    previous_bytes = rendered.bytes;

                    if rendered.actionable_elements == 0 {
                        counts.unactionable_views += 1;
                    }
                    if rendered.ambiguous_candidates > 0 {
                        counts.ambiguous_views += 1;
                    }

                    // The adapter runs against exactly the bytes a provider
                    // would have received. Its output carries no authority at
                    // all: only `accept_model_proposal` against a live record
                    // can turn it into anything, and that path is covered by
                    // `computer_use_sealed_boundary.rs`.
                    let raw = respond(&payload, repeat);
                    assert!(
                        !raw.is_empty(),
                        "{adapter_name}/{profile}: adapter produced nothing"
                    );
                }
            }
        }
    }
    counts
}

#[test]
fn deterministic_campaign_is_redaction_safe_and_correctly_accounted() {
    let counts = run_campaign();
    assert_eq!(
        counts.episodes,
        FRAMES.len() * ADAPTERS.len() * REPEATS * AdaptiveProfile::ALL.len()
    );
    assert_eq!(counts.episodes, 300, "campaign denominator changed");
    assert_eq!(
        counts.redaction_leaks, 0,
        "a rendered observation leaked content no profile may show a model"
    );
    assert_eq!(
        counts.accounting_mismatches, 0,
        "accounted observation bytes disagreed with the bytes actually rendered"
    );
    assert_eq!(
        counts.monotonicity_violations, 0,
        "a cheaper profile rendered more than a richer one"
    );
    // The campaign is only meaningful if it actually exercised both signals.
    assert!(
        counts.unactionable_views > 0,
        "no frame exercised the missing-semantics path"
    );
    assert!(
        counts.ambiguous_views > 0,
        "no frame exercised the duplicate-name ambiguity path"
    );
}

#[test]
fn the_campaign_is_reproducible() {
    assert_eq!(
        run_campaign(),
        run_campaign(),
        "the campaign is not deterministic"
    );
}

#[test]
fn a_hard_denied_element_is_invisible_to_every_profile() {
    let observation = sensitive_frame();
    for profile in AdaptiveProfile::ALL {
        let (payload, rendered) = render_computer_observation(&observation, profile);
        let text = payload.to_string();
        assert!(!text.contains("CREDENTIAL-NEEDLE"), "{profile}: {text}");
        assert!(!text.contains("password"), "{profile}: {text}");
        assert_eq!(rendered.rendered_elements, 1, "{profile}");
    }
}

#[test]
fn duplicate_accessible_names_are_detected_in_every_profile() {
    let observation = duplicate_label_frame();
    for profile in AdaptiveProfile::ALL {
        let (_payload, rendered) = render_computer_observation(&observation, profile);
        assert_eq!(
            rendered.ambiguous_candidates, 2,
            "{profile} did not see the duplicate accessible names"
        );
    }
}

#[test]
fn a_disabled_only_surface_offers_nothing_to_any_profile() {
    let observation = missing_semantics_frame();
    for profile in AdaptiveProfile::ALL {
        let (_payload, rendered) = render_computer_observation(&observation, profile);
        assert_eq!(rendered.actionable_elements, 0, "{profile}");
    }
}

#[test]
fn economy_costs_strictly_less_than_high_assurance_and_shows_a_subset() {
    let mut wide = unique_semantic_frame();
    wide.elements = (0..400)
        .map(|index| {
            element(
                &format!("control-{index:04}"),
                "Control",
                SemanticAction::Invoke,
                true,
                index == 3,
            )
        })
        .collect();
    let (economy_payload, economy) = render_computer_observation(&wide, AdaptiveProfile::Economy);
    let (_, balanced) = render_computer_observation(&wide, AdaptiveProfile::Balanced);
    let (high_payload, high) = render_computer_observation(&wide, AdaptiveProfile::HighAssurance);
    assert!(economy.bytes < balanced.bytes);
    assert!(balanced.bytes < high.bytes);
    assert!(economy.truncated && !high.truncated);

    let ids = |payload: &serde_json::Value| -> Vec<String> {
        payload["elements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|element| element["element_id"].as_str().unwrap().to_string())
            .collect()
    };
    let economy_ids = ids(&economy_payload);
    let high_ids = ids(&high_payload);
    assert!(
        economy_ids.iter().all(|id| high_ids.contains(id)),
        "the cheap view showed a control the rich view did not"
    );
}

#[test]
fn every_profile_stays_inside_its_own_byte_ceiling() {
    let mut huge = unique_semantic_frame();
    huge.elements = (0..900)
        .map(|index| {
            let mut element = element(
                &format!("el-{index:04}"),
                "Control",
                SemanticAction::Invoke,
                true,
                false,
            );
            element.label = Some("L".repeat(500));
            element.value = Some("V".repeat(500));
            element
        })
        .collect();
    for profile in AdaptiveProfile::ALL {
        let budget = profile.budget();
        let (_, rendered) = render_computer_observation(&huge, profile);
        assert!(
            rendered.bytes <= budget.max_observation_bytes,
            "{profile} rendered {} bytes over a {} ceiling",
            rendered.bytes,
            budget.max_observation_bytes
        );
        assert!(rendered.truncated, "{profile} should report a bounded view");
    }
}

#[test]
fn rendering_is_byte_stable_and_independent_of_input_order() {
    let frame = unique_semantic_frame();
    let mut shuffled = frame.clone();
    shuffled.elements.reverse();
    for profile in AdaptiveProfile::ALL {
        let first = render_computer_observation(&frame, profile);
        let second = render_computer_observation(&frame, profile);
        assert_eq!(first.0, second.0, "{profile} is not byte-stable");
        assert_eq!(first.1, second.1, "{profile} accounting is not stable");
        assert_eq!(
            first.0,
            render_computer_observation(&shuffled, profile).0,
            "{profile} depends on host element order"
        );
    }
}

#[test]
fn only_the_richest_profile_sees_geometry_or_a_capture_reference() {
    let frame = unique_semantic_frame();
    let (economy, _) = render_computer_observation(&frame, AdaptiveProfile::Economy);
    assert!(!economy.to_string().contains("bounds"));
    assert!(economy.get("geometry").is_none());
    assert!(economy.get("screenshot").is_none());

    let (balanced, _) = render_computer_observation(&frame, AdaptiveProfile::Balanced);
    assert!(balanced.to_string().contains("bounds"));
    assert!(balanced.get("geometry").is_some());
    assert!(balanced.get("screenshot").is_none());

    let (high, _) = render_computer_observation(&frame, AdaptiveProfile::HighAssurance);
    assert_eq!(high["screenshot"]["captured"], serde_json::json!(false));
}

/// Cross-lane contract with the standalone evaluation harness (#446/#448).
///
/// That lane published the naming decision this seam adopts: canonical
/// `economy` / `balanced` / `high_assurance`, with `efficient` and `frontier`
/// accepted on ingest and canonicalized. The evaluator crate is not in this
/// workspace, so the contract is asserted here against the exact tokens from
/// its decision packet rather than by importing it.
#[test]
fn the_published_evaluation_naming_contract_holds_in_production() {
    let canonical: Vec<&str> = AdaptiveProfile::ALL
        .iter()
        .map(|profile| profile.as_str())
        .collect();
    assert_eq!(canonical, vec!["economy", "balanced", "high_assurance"]);

    for (alias, expected) in [
        ("efficient", AdaptiveProfile::Economy),
        ("frontier", AdaptiveProfile::HighAssurance),
    ] {
        let ingested: AdaptiveProfile =
            serde_json::from_str(&format!("\"{alias}\"")).expect("alias ingests");
        assert_eq!(ingested, expected, "{alias} canonicalized wrongly");
        assert_eq!(
            serde_json::to_string(&ingested).unwrap(),
            format!("\"{}\"", expected.as_str()),
            "an alias was written back out"
        );
    }

    // Aliasing carries identity, not the donor's semantics: #453's `Frontier`
    // disabled the host verification its `Efficient` required, so a lexical
    // rename would have inverted the assurance ladder. Every profile shares one
    // floor here, which is what makes the alias safe to accept.
    let floors: BTreeSet<String> = AdaptiveProfile::ALL
        .iter()
        .map(|profile| serde_json::to_string(&profile.safety_floor()).unwrap())
        .collect();
    assert_eq!(
        floors.len(),
        1,
        "a profile obtained a different safety floor"
    );
}
