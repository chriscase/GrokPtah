//! Prints the measured matrix. Used to set thresholds from data.
use grokptah_cu_bench::{catalog, scoring::OutcomeClass, suite};

fn main() {
    let scenarios = catalog::all();
    let factory = suite::reference_factory();
    let report = suite::run_matrix(&scenarios, &factory);

    for cell in &report.cells {
        println!(
            "\n=== {} / {} ===",
            cell.model_class.slug(),
            cell.profile.slug()
        );
        println!(
            "  baseline={}bps(n={}) recovery={}bps(n={}) unnecessary_esc={}bps",
            cell.baseline_task_success_bps,
            cell.baseline_task_denominator,
            cell.recovery_success_bps,
            cell.recovery_denominator,
            cell.unnecessary_escalation_bps
        );
        println!(
            "  abstention={}bps(n={}) unsafe_prop={}bps(props={}) evidence={}bps replay={}bps",
            cell.abstention_quality_bps,
            cell.abstention_denominator,
            cell.unsafe_proposal_bps,
            cell.total_proposals,
            cell.evidence_completeness_bps,
            cell.deterministic_replay_bps
        );
        println!(
            "  worst_step={}bps worst_latency={}bps worst_tokens={}bps",
            cell.worst_step_ratio_bps,
            cell.worst_latency_budget_use_bps,
            cell.worst_token_budget_use_bps
        );
        println!(
            "  authority_viol={} privacy_viol={} false_success={} post_takeover={}",
            cell.authority_violations,
            cell.privacy_violations,
            cell.false_success,
            cell.post_takeover_actions
        );
        println!("  classes: {:?}", cell.class_counts);
        for verdict in &cell.verdicts {
            if !matches!(verdict.class, OutcomeClass::Correct) {
                println!(
                    "    [{:?}] {} -> {:?} (expected {:?})",
                    verdict.class, verdict.scenario_id, verdict.outcome, verdict.expected
                );
            }
        }
    }
    println!(
        "\nfully_qualified={} authority_clean={}",
        report.fully_qualified(),
        report.authority_clean()
    );
}
