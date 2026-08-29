//! The structural gate for the one physical provider-send chokepoint (#478).
//!
//! # Why this shape
//!
//! The obvious gate — "check that every caller of the two transport helpers is
//! bound" — reports full coverage while missing the worst case. The provider
//! qualification probes built their own `reqwest` client, POSTed to
//! `/chat/completions` themselves, and never called either helper, all while
//! spending the operator's credential on real model invocations. A gate keyed
//! on *calling a helper* cannot see them.
//!
//! So this gate keys on **constructing a provider request**: the HTTP client,
//! the completions URL, and the POST. Anything that could reach inference has
//! to do one of those three things, and all three are allowed in exactly one
//! module.
//!
//! The compiler already enforces the other half: `provider_send::dispatch`
//! takes a `ProviderSendContext`, and `ResponseReader` never hands out the raw
//! `reqwest::Response`, so an unbound send does not typecheck. This gate covers
//! what types cannot: a *new* client built somewhere else.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The one module allowed to construct a provider inference request.
const CHOKEPOINT: &str = "src/provider_send/transport.rs";

/// Files allowed to build a `reqwest` client for something that is *not*
/// provider inference. Each entry states why, because adding one is the way a
/// future unbound send would get in.
const NON_INFERENCE_CLIENTS: &[(&str, &str)] = &[
    (
        "src/provider_discovery.rs",
        "GET model catalogue listing; never a completion",
    ),
    (
        "src/auth_store.rs",
        "OIDC token endpoint; never a completion",
    ),
    (
        "src/host_helpers.rs",
        "web_fetch tool against an arbitrary user URL; never a completion",
    ),
    (
        "src/mcp_control_client.rs",
        "loopback MCP control plane; never a completion",
    ),
];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// One production (non-test) line of source.
struct Line {
    file: String,
    number: usize,
    text: String,
}

/// Read a file with `#[cfg(test)]` modules removed.
///
/// Test code legitimately builds its own clients to *be* a provider, so the
/// gate would be pure noise without this.
fn production_lines(path: &Path, root: &Path) -> Vec<Line> {
    let relative = path
        .strip_prefix(root)
        .expect("path under crate root")
        .to_string_lossy()
        .replace('\\', "/");
    let text = std::fs::read_to_string(path).expect("read source");
    let mut out = Vec::new();
    let mut skip_until: Option<String> = None;
    let mut pending_cfg_test: Option<String> = None;

    for (index, line) in text.lines().enumerate() {
        if let Some(closing) = skip_until.as_ref() {
            if line == closing {
                skip_until = None;
            }
            continue;
        }
        let indent: String = line.chars().take_while(|c| *c == ' ').collect();
        if line.trim() == "#[cfg(test)]" {
            pending_cfg_test = Some(indent);
            continue;
        }
        if let Some(indent) = pending_cfg_test.take() {
            if line.trim_start().starts_with("mod ") && line.trim_end().ends_with('{') {
                skip_until = Some(format!("{indent}}}"));
                continue;
            }
            // `#[cfg(test)]` on something other than an inline module (a `use`,
            // a `#[path]` module, an item): the attribute line is skipped and
            // this line is judged normally.
        }
        out.push(Line {
            file: relative.clone(),
            number: index + 1,
            text: line.to_string(),
        });
    }
    out
}

fn all_production_lines() -> Vec<Line> {
    let root = crate_root();
    let mut files = Vec::new();
    rust_sources(&root.join("src"), &mut files);
    files.sort();
    let mut out = Vec::new();
    for file in files {
        out.extend(production_lines(&file, &root));
    }
    out
}

fn violations(lines: &[Line], matches: impl Fn(&str) -> bool, allowed: &[&str]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| matches(&line.text))
        .filter(|line| !allowed.contains(&line.file.as_str()))
        .map(|line| format!("{}:{}: {}", line.file, line.number, line.text.trim()))
        .collect()
}

#[test]
fn only_the_chokepoint_builds_an_inference_http_client() {
    let lines = all_production_lines();
    let mut allowed: Vec<&str> = NON_INFERENCE_CLIENTS
        .iter()
        .map(|(file, _)| *file)
        .collect();
    allowed.push(CHOKEPOINT);

    let found = violations(
        &lines,
        |text| text.contains("reqwest::Client::builder") || text.contains("reqwest::Client::new"),
        &allowed,
    );
    assert!(
        found.is_empty(),
        "a provider-capable HTTP client may only be built in {CHOKEPOINT}.\n\
         Building one elsewhere is how an unbound send site gets in — that is exactly \
         how the qualification probes reached a model without ever being recorded.\n\
         If the new client genuinely cannot reach inference, add it to \
         NON_INFERENCE_CLIENTS with a reason.\nOffending lines:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn only_the_chokepoint_constructs_a_completions_url() {
    let lines = all_production_lines();
    // A *constructed* URL, not a route template or a doc comment: the gate
    // looks for the path appearing inside string interpolation or a push.
    let found = violations(
        &lines,
        |text| {
            let trimmed = text.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") {
                return false;
            }
            text.contains("chat/completions")
                && (text.contains("format!") || text.contains("push_str"))
        },
        &[CHOKEPOINT],
    );
    assert!(
        found.is_empty(),
        "the provider completions URL may only be constructed in {CHOKEPOINT}.\n\
         Offending lines:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn only_the_chokepoint_posts_to_a_provider() {
    let lines = all_production_lines();
    let mut allowed: Vec<&str> = NON_INFERENCE_CLIENTS
        .iter()
        .map(|(file, _)| *file)
        .collect();
    allowed.push(CHOKEPOINT);

    let found = violations(
        &lines,
        |text| {
            let trimmed = text.trim_start();
            // `.route("...", post(handler))` is an axum server route, not a
            // client request.
            !trimmed.starts_with("//") && text.contains(".post(") && !text.contains(".route(")
        },
        &allowed,
    );
    assert!(
        found.is_empty(),
        "an outbound POST that could reach inference may only be built in {CHOKEPOINT}.\n\
         Offending lines:\n  {}",
        found.join("\n  ")
    );
}

/// The certification recorder is fed only from the chokepoint.
///
/// It records one row per physical attempt, which is exactly the shape of a
/// second send ledger — so the guarantee that keeps it a *projection* is that
/// the only thing that can feed it is the adapter the chokepoint calls.
#[test]
fn the_observation_recorder_is_fed_only_from_the_chokepoint() {
    let lines = all_production_lines();
    let feeders: Vec<_> = lines
        .iter()
        .filter(|line| {
            let trimmed = line.text.trim_start();
            !trimmed.starts_with("//")
                && (trimmed.starts_with("record_provider_attempt(")
                    || trimmed.contains(".begin_observation()"))
        })
        .map(|line| format!("{}:{}: {}", line.file, line.number, line.text.trim()))
        .collect();
    assert_eq!(
        feeders.len(),
        2,
        "the observation recorder must be fed from exactly one adapter (its \
         `begin_observation` and its `record_provider_attempt`), both driven by the \
         chokepoint. Anything else is a second ledger tracking the same sends.\n\
         Found:\n  {}",
        feeders.join("\n  ")
    );
    for feeder in &feeders {
        assert!(
            feeder.starts_with("src/host_helpers.rs"),
            "observation is fed from an unexpected place: {feeder}"
        );
    }
}

#[test]
fn the_send_lattice_is_the_only_send_ledger() {
    let lines = all_production_lines();
    // These identifiers name the one durable send truth. A second definition
    // anywhere else is, by construction, a second ledger.
    for identifier in [
        "ProviderAttemptState",
        "enum ProviderAttempt",
        "struct ProviderAttempt ",
        "fn mark_sending",
        "fn begin_attempt",
    ] {
        let found: Vec<_> = lines
            .iter()
            .filter(|line| line.text.contains(identifier))
            .filter(|line| !line.file.starts_with("src/provider_send/"))
            // A caller *using* the context is fine; only a definition is not.
            .filter(|line| {
                let trimmed = line.text.trim_start();
                trimmed.starts_with("pub fn")
                    || trimmed.starts_with("fn ")
                    || trimmed.starts_with("pub enum")
                    || trimmed.starts_with("enum ")
                    || trimmed.starts_with("pub struct")
                    || trimmed.starts_with("struct ")
            })
            .map(|line| format!("{}:{}: {}", line.file, line.number, line.text.trim()))
            .collect();
        assert!(
            found.is_empty(),
            "`{identifier}` is the one send lattice's vocabulary; a second definition is a \
             second send ledger.\nOffending lines:\n  {}",
            found.join("\n  ")
        );
    }
}

#[test]
fn production_code_never_arms_a_crash_cut() {
    let lines = all_production_lines();
    let found = violations(
        &lines,
        |text| {
            let trimmed = text.trim_start();
            !trimmed.starts_with("//")
                && (text.contains("arm_crash_cut(") || text.contains("disarm_crash_cut("))
        },
        // The definitions themselves live in the crash module.
        &["src/provider_send/crash.rs"],
    );
    assert!(
        found.is_empty(),
        "crash cuts are a test facility; production code must never arm one.\n\
         Offending lines:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn every_call_site_family_is_actually_wired() {
    use grokptah_agent_bridge::provider_send::CallSiteFamily;
    let lines = all_production_lines();
    let mentioned: BTreeSet<String> = lines
        .iter()
        .filter(|line| !line.file.starts_with("src/provider_send/"))
        .flat_map(|line| {
            CallSiteFamily::ALL
                .iter()
                .filter(|family| {
                    line.text
                        .contains(&format!("CallSiteFamily::{}", variant_name(**family)))
                })
                .map(|family| variant_name(*family).to_string())
                .collect::<Vec<_>>()
        })
        .collect();

    let missing: Vec<_> = CallSiteFamily::ALL
        .iter()
        .map(|family| variant_name(*family))
        .filter(|name| !mentioned.contains(*name))
        .collect();
    assert!(
        missing.is_empty(),
        "these call-site families are declared but never opened by any production path, \
         so nothing is actually bound to them: {missing:?}"
    );
}

#[test]
fn every_send_origin_is_actually_wired() {
    use grokptah_agent_bridge::provider_send::SendOrigin;
    let lines = all_production_lines();
    let wired: BTreeSet<&str> = SendOrigin::ALL
        .iter()
        .map(|origin| origin_variant(*origin))
        .filter(|name| {
            lines.iter().any(|line| {
                !line.file.starts_with("src/provider_send/")
                    && line.text.contains(&format!("SendOrigin::{name}"))
            })
        })
        .collect();

    // Desktop, Orchestration, and Qualification are the origins this build can
    // actually distinguish at the send site. The headless service, the MCP
    // broker, scheduled routines, and manager work all reach a model through an
    // orchestration run, so they are recorded as `Orchestration` until run
    // provenance lands (#461 queue ownership carries it).
    for required in ["Desktop", "Orchestration", "Qualification"] {
        assert!(
            wired.contains(required),
            "SendOrigin::{required} is never opened by any production path"
        );
    }
}

fn variant_name(family: grokptah_agent_bridge::provider_send::CallSiteFamily) -> &'static str {
    use grokptah_agent_bridge::provider_send::CallSiteFamily as F;
    match family {
        F::DesktopChatTurn => "DesktopChatTurn",
        F::DesktopBuildRound => "DesktopBuildRound",
        F::PlanProposal => "PlanProposal",
        F::SessionCompaction => "SessionCompaction",
        F::ExploreSubagent => "ExploreSubagent",
        F::GeneralPurposeSubagent => "GeneralPurposeSubagent",
        F::ComputerUseRound => "ComputerUseRound",
        F::ComputerUseQualification => "ComputerUseQualification",
        F::ProviderQualificationProbe => "ProviderQualificationProbe",
    }
}

fn origin_variant(origin: grokptah_agent_bridge::provider_send::SendOrigin) -> &'static str {
    use grokptah_agent_bridge::provider_send::SendOrigin as O;
    match origin {
        O::Desktop => "Desktop",
        O::Orchestration => "Orchestration",
        O::HeadlessService => "HeadlessService",
        O::McpBroker => "McpBroker",
        O::ScheduledRoutine => "ScheduledRoutine",
        O::Manager => "Manager",
        O::Qualification => "Qualification",
    }
}

#[test]
fn the_gate_would_catch_the_qualification_probe_regression() {
    // A guard on the guard: the historical miss was a file that built its own
    // client and POSTed a completions URL. Re-create that shape as text and
    // confirm each rule fires on it.
    let offender = vec![
        Line {
            file: "src/provider_qualification.rs".into(),
            number: 1,
            text: "    let client = reqwest::Client::builder().build()?;".into(),
        },
        Line {
            file: "src/provider_qualification.rs".into(),
            number: 2,
            text: r#"    let url = format!("{}/chat/completions", base_url);"#.into(),
        },
        Line {
            file: "src/provider_qualification.rs".into(),
            number: 3,
            text: "    let request = client.post(&url);".into(),
        },
    ];

    // provider_qualification.rs is *not* in NON_INFERENCE_CLIENTS, so the
    // client rule fires.
    let mut allowed: Vec<&str> = NON_INFERENCE_CLIENTS
        .iter()
        .map(|(file, _)| *file)
        .collect();
    allowed.push(CHOKEPOINT);
    assert_eq!(
        violations(
            &offender,
            |text| text.contains("reqwest::Client::builder")
                || text.contains("reqwest::Client::new"),
            &allowed,
        )
        .len(),
        1,
        "the client rule must fire on the historical regression"
    );
    assert_eq!(
        violations(
            &offender,
            |text| text.contains("chat/completions") && text.contains("format!"),
            &[CHOKEPOINT],
        )
        .len(),
        1,
        "the URL rule must fire on the historical regression"
    );
    assert_eq!(
        violations(
            &offender,
            |text| text.contains(".post(") && !text.contains(".route("),
            &allowed,
        )
        .len(),
        1,
        "the POST rule must fire on the historical regression"
    );
}

#[test]
fn the_non_inference_allowlist_stays_small_and_explained() {
    for (file, reason) in NON_INFERENCE_CLIENTS {
        assert!(
            crate_root().join(file).exists(),
            "allowlisted file {file} no longer exists; drop the stale entry"
        );
        assert!(
            reason.len() > 20,
            "allowlist entry {file} needs a real reason, not `{reason}`"
        );
    }
    assert!(
        NON_INFERENCE_CLIENTS.len() <= 6,
        "the non-inference allowlist is growing; each entry is a place a future \
         provider send could hide"
    );
}
