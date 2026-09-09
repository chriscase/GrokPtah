//! Sep 18 One-Mac / CI checklist runner + independent evidence-pack verifier.

use std::env;
use std::path::PathBuf;
use std::process;

use grokptah_isolated_surface::{
    parse_evidence_pack, parse_sep18_checklist_run_args, run_sep18_checklist, verifier_exit_code,
    verify_evidence_pack, HostSentinelSnapshot,
};

fn usage() -> ! {
    eprintln!(
        "Usage:\n\
          grokptah-sep18-checklist run [--output PATH] [--vf-dry-run [--native-host-sentinels]] [--fault-matrix CASE]\n\
          grokptah-sep18-checklist verify PATH\n\
         \n\
         Default substrate: Contained Browser dry-run (Linux CI / one-Mac rehearsal).\n\
         --native-host-sentinels is valid only with --vf-dry-run; unavailable native readings fail closed.\n\
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
    let request = parse_sep18_checklist_run_args(args).unwrap_or_else(|err| {
        eprintln!("{err}");
        usage();
    });

    let outcome = run_sep18_checklist(HostSentinelSnapshot::synthetic_baseline(), request.config);
    if let Some(err) = &outcome.runner_error {
        eprintln!("runner error: {err:?}");
        process::exit(1);
    }

    if let Err(err) = outcome.write_pack(&request.output) {
        eprintln!("failed to write evidence pack: {err:?}");
        process::exit(1);
    }

    let decision = verify_evidence_pack(&outcome.pack);
    eprintln!(
        "wrote {} — verifier: {} ({:?})",
        request.output.display(),
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
