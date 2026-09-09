//! Sep 18 One-Mac / CI checklist runner + independent evidence-pack verifier.

use std::env;
use std::path::PathBuf;
use std::process;

use grokptah_isolated_surface::{
    parse_evidence_pack, run_sep18_checklist, verifier_exit_code, verify_evidence_pack,
    FaultMatrixCase, HostSentinelSnapshot, Sep18ChecklistRunnerConfig, Sep18ChecklistSubstrate,
};

fn usage() -> ! {
    eprintln!(
        "Usage:\n\
          grokptah-sep18-checklist run [--output PATH] [--vf-dry-run] [--native-host-sentinels] [--checkout PATH] [--fault-matrix CASE]\n\
          grokptah-sep18-checklist verify PATH\n\
         \n\
         Default substrate: Contained Browser dry-run (Linux CI / one-Mac rehearsal).\n\
         --native-host-sentinels is exclusive (not combined with --vf-dry-run, --synthetic, or --fault-matrix).\n\
         Admission stays false; physical_pass_claimed=false unless a future physical gate flips it."
    );
    process::exit(2);
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        usage();
    }

    match args[1].as_str() {
        "run" => exit_run(&args[2..]),
        "verify" => exit_verify(&args[2..]),
        "--help" | "-h" | "help" => usage(),
        _ => usage(),
    }
}

fn exit_run(args: &[String]) {
    let mut output = PathBuf::from("sep18-evidence-pack.json");
    let mut config = Sep18ChecklistRunnerConfig::contained_browser_default();
    let mut native_host_sentinels = false;
    let mut vf_dry_run = false;
    let mut synthetic = false;
    let mut checkout: Option<PathBuf> = None;

    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--output" | "-o" => {
                idx += 1;
                output = PathBuf::from(args.get(idx).unwrap_or_else(|| usage()));
            }
            "--vf-dry-run" => {
                vf_dry_run = true;
            }
            "--synthetic" => {
                synthetic = true;
            }
            "--native-host-sentinels" => {
                native_host_sentinels = true;
            }
            "--checkout" => {
                idx += 1;
                checkout = Some(PathBuf::from(args.get(idx).unwrap_or_else(|| usage())));
            }
            "--fault-matrix" => {
                idx += 1;
                let case = args.get(idx).unwrap_or_else(|| usage());
                config.fault_matrix_case = Some(parse_fault_matrix(case));
            }
            other => {
                eprintln!("unknown run flag: {other}");
                usage();
            }
        }
        idx += 1;
    }

    if native_host_sentinels && (vf_dry_run || synthetic || config.fault_matrix_case.is_some()) {
        eprintln!(
            "--native-host-sentinels cannot be combined with --vf-dry-run, --synthetic, or --fault-matrix"
        );
        usage();
    }

    if vf_dry_run {
        config = Sep18ChecklistRunnerConfig::vf_dry_run("sep18-cli-vf-dry-run");
    } else if synthetic {
        config.substrate = Sep18ChecklistSubstrate::SyntheticHarness;
    } else if native_host_sentinels {
        config = Sep18ChecklistRunnerConfig::native_host_sentinel(
            checkout.clone().unwrap_or_else(|| PathBuf::from(".")),
        );
    }

    if let Some(path) = checkout {
        if !native_host_sentinels {
            eprintln!("--checkout is only valid with --native-host-sentinels");
            usage();
        }
        config.checkout_path = Some(path);
    }

    let outcome = run_sep18_checklist(HostSentinelSnapshot::synthetic_baseline(), config);
    if let Some(err) = &outcome.runner_error {
        eprintln!("runner error: {err:?}");
        process::exit(1);
    }

    if let Err(err) = outcome.write_pack(&output) {
        eprintln!("failed to write evidence pack: {err:?}");
        process::exit(1);
    }

    let decision = verify_evidence_pack(&outcome.pack);
    eprintln!(
        "wrote {} — verifier: {} ({:?})",
        output.display(),
        if decision.accepted {
            "accepted"
        } else {
            "rejected"
        },
        decision.code
    );
    if !decision.accepted {
        eprintln!("{}", decision.message);
        process::exit(verifier_exit_code(&decision));
    }
}

fn exit_verify(args: &[String]) {
    let path = args.first().map(PathBuf::from).unwrap_or_else(|| usage());
    let json = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        eprintln!("failed to read {}: {err}", path.display());
        process::exit(1);
    });
    let pack = parse_evidence_pack(&json).unwrap_or_else(|err| {
        eprintln!("failed to parse evidence pack: {err}");
        process::exit(1);
    });
    let decision = verify_evidence_pack(&pack);
    eprintln!(
        "{}: {} ({:?}) — {}",
        path.display(),
        if decision.accepted {
            "accepted"
        } else {
            "rejected"
        },
        decision.code,
        decision.message
    );
    process::exit(verifier_exit_code(&decision));
}

fn parse_fault_matrix(value: &str) -> FaultMatrixCase {
    match value {
        "boot_stop" | "BootStop" => FaultMatrixCase::BootStop,
        "pre_dispatch_stop" | "PreDispatchStop" => FaultMatrixCase::PreDispatchStop,
        "lost_ack_uncertain" | "LostAckUncertain" => FaultMatrixCase::LostAckUncertain,
        "restart_no_replay" | "RestartNoReplay" => FaultMatrixCase::RestartNoReplay,
        other => {
            eprintln!("unknown fault matrix case: {other}");
            usage();
        }
    }
}
