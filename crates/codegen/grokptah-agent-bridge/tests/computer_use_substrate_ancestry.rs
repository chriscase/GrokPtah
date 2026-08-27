//! Assembly guard for the Computer Use substrate ancestry.
//!
//! This tree (`claude/grokptah-packaged-qualification-vqk7rd` lane) and the
//! isolated-guest lane on PR #439 are genuine forks: their merge-base is
//! `127ffaff`, and neither is an ancestor of the other. They carry two
//! *different* run-control authorities for the same subsystem:
//!
//! * this lane: `computer_use/control.rs` — `ComputerClientIdentity`,
//!   `ComputerGrantRequest`, `ComputerAgentObservation`, `ComputerRunController`,
//!   `ComputerRunAgentController`, wired into `lib.rs`, `mcp_control.rs`,
//!   `host.rs` and `orchestration/service.rs`;
//! * PR #439: `computer_use/coordination.rs` — `ComputerSurfaceLease`,
//!   `ComputerSurfaceLeaseState`, `ComputerDispatchRecord`, `ComputerDispatchState`,
//!   `HostSurfaceLeaseRequest`, `HostLeasePriority`. None of this lane's five
//!   symbols survives there.
//!
//! Landing one lane's modules on top of the other's authority would stand up a
//! second identity/supervision system for one subsystem. These tests read the
//! sources rather than compiling them, so they hold on every host — including
//! the macOS-only substrate a Linux `--all-targets` build never sees.
//!
//! See `docs/evidence/CU_SUBSTRATE_ANCESTRY_RESIDUAL.md` for the measured DAG.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Run-control authority modules. Exactly one may exist at a time.
const CONTROL_AUTHORITY: &str = "control.rs";
const COORDINATION_AUTHORITY: &str = "coordination.rs";

/// The run-control surface this lane re-exports from `control.rs`.
const GATE_NATIVE_CONTROL_SYMBOLS: &[&str] = &[
    "ComputerAgentObservation",
    "ComputerClientIdentity",
    "ComputerGrantRequest",
    "ComputerRunAgentController",
    "ComputerRunController",
];

fn computer_use_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/computer_use")
}

fn module_files() -> BTreeSet<String> {
    fs::read_dir(computer_use_dir())
        .expect("computer_use module directory is readable")
        .map(|entry| entry.expect("directory entry").file_name())
        .filter_map(|name| name.to_str().map(str::to_owned))
        .filter(|name| name.ends_with(".rs"))
        .collect()
}

fn mod_rs() -> String {
    fs::read_to_string(computer_use_dir().join("mod.rs")).expect("computer_use/mod.rs is readable")
}

/// Module names declared by `mod X;` in `computer_use/mod.rs`, `cfg`-gated or not.
fn declared_modules(source: &str) -> BTreeSet<String> {
    source
        .lines()
        .map(str::trim)
        .filter_map(|line| {
            let rest = line
                .strip_prefix("mod ")
                .or_else(|| line.strip_prefix("pub mod "))
                .or_else(|| line.strip_prefix("pub(crate) mod "))?;
            rest.strip_suffix(';').map(str::to_owned)
        })
        .collect()
}

/// Exactly one run-control authority may be assembled into the subsystem.
///
/// Both present is the reconciliation hazard: two lease/identity models
/// supervising one Computer Run. Neither present means the authority was
/// deleted outright.
#[test]
fn exactly_one_run_control_authority_is_assembled() {
    let files = module_files();
    let has_control = files.contains(CONTROL_AUTHORITY);
    let has_coordination = files.contains(COORDINATION_AUTHORITY);

    assert!(
        has_control || has_coordination,
        "computer_use has no run-control authority: expected exactly one of {CONTROL_AUTHORITY} \
         or {COORDINATION_AUTHORITY}, found neither"
    );
    assert!(
        !(has_control && has_coordination),
        "computer_use carries BOTH {CONTROL_AUTHORITY} (this lane: run controllers + client \
         identity + grant requests) and {COORDINATION_AUTHORITY} (PR #439: surface leases + \
         dispatch). That is two identity/supervision systems for one subsystem. Adopting the \
         PR #439 authority must remove control.rs and its re-exports in the same change."
    );
}

/// The gate-native control surface is re-exported exactly once, from the
/// authority module, and is not shadowed by a second definition.
#[test]
fn gate_native_control_surface_has_a_single_definition_site() {
    let files = module_files();
    if !files.contains(CONTROL_AUTHORITY) {
        // The PR #439 authority is in force; this lane's surface is expected to
        // be gone. `exactly_one_run_control_authority_is_assembled` owns that case.
        return;
    }

    let source = mod_rs();
    assert!(
        source.contains("pub use control::{"),
        "control.rs is present but computer_use/mod.rs does not re-export it"
    );

    for symbol in GATE_NATIVE_CONTROL_SYMBOLS {
        assert!(
            source.contains(symbol),
            "computer_use/mod.rs no longer re-exports `{symbol}`; lib.rs, mcp_control.rs, \
             host.rs and orchestration/service.rs consume it"
        );

        // No other module in the subsystem may define the same symbol.
        let definers: Vec<String> = files
            .iter()
            .filter(|name| name.as_str() != CONTROL_AUTHORITY && name.as_str() != "mod.rs")
            .filter(|name| {
                let body = fs::read_to_string(computer_use_dir().join(name)).unwrap_or_default();
                body.contains(&format!("pub struct {symbol}"))
                    || body.contains(&format!("pub enum {symbol}"))
                    || body.contains(&format!("pub trait {symbol}"))
            })
            .cloned()
            .collect();
        assert!(
            definers.is_empty(),
            "`{symbol}` is defined by control.rs and also by {definers:?}; a second definition \
             site is a second supervision system"
        );
    }
}

/// The isolated-visual substrate is built on the PR #439 lease/dispatch model.
/// It may not be assembled over this lane's controller authority.
#[test]
fn isolated_visual_substrate_requires_the_coordination_authority() {
    let files = module_files();
    let isolated: Vec<&String> = files
        .iter()
        .filter(|name| name.starts_with("isolated_") || name.starts_with("macos_isolated_"))
        .collect();

    if isolated.is_empty() {
        return; // This ancestry point carries none of it.
    }

    assert!(
        files.contains(COORDINATION_AUTHORITY),
        "isolated-visual modules {isolated:?} were assembled while {CONTROL_AUTHORITY} is still \
         the run-control authority. That substrate is bound to {COORDINATION_AUTHORITY}'s lease \
         and dispatch model; landing it over this lane's controllers mixes two supervision \
         models. Port the authority first."
    );
}

/// A partially applied port — files copied without wiring, or wiring without
/// files — fails here rather than at a later host-specific build.
#[test]
fn every_module_is_declared_and_every_declaration_resolves() {
    let files = module_files();
    let declared = declared_modules(&mod_rs());

    let on_disk: BTreeSet<String> = files
        .iter()
        .filter(|name| name.as_str() != "mod.rs")
        .map(|name| name.trim_end_matches(".rs").to_owned())
        .collect();

    let undeclared: Vec<&String> = on_disk.difference(&declared).collect();
    assert!(
        undeclared.is_empty(),
        "computer_use module files exist but are not declared in mod.rs: {undeclared:?}. \
         A copied-but-unwired module is a half-applied port."
    );

    let unresolved: Vec<&String> = declared.difference(&on_disk).collect();
    assert!(
        unresolved.is_empty(),
        "computer_use/mod.rs declares modules with no source file: {unresolved:?}"
    );
}

/// Adopting the other lane must not silently drop this lane's MCP mutation gate.
#[test]
fn mcp_mutation_gate_survives_while_this_lane_is_the_authority() {
    if !module_files().contains(CONTROL_AUTHORITY) {
        return;
    }
    let gate = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mcp_computer_mutations.rs");
    assert!(
        gate.is_file(),
        "control.rs is the run-control authority but tests/mcp_computer_mutations.rs is gone; \
         the MCP mutation surface would be ungated"
    );
}
