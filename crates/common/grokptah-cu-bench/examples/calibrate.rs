//! Prints the measured matrix for the reference agent and every calibration
//! tier. Used to set thresholds from data rather than from guesswork.
use grokptah_cu_bench::agent::{Agent, ReferenceAgent};
use grokptah_cu_bench::calibration::CalibrationTier;
use grokptah_cu_bench::modelclass::ModelClass;
use grokptah_cu_bench::scenario::Scenario;
use grokptah_cu_bench::scoring::OutcomeClass;
use grokptah_cu_bench::{catalog, suite};

fn dump(label: &str, factory: &dyn Fn(ModelClass, &Scenario) -> Box<dyn Agent>, verbose: bool) {
    let scenarios = catalog::all();
    let report = suite::run_matrix(&scenarios, factory);
    println!("\n######## {label} ########");
    for cell in &report.cells {
        let q = report
            .qualification(cell.model_class, cell.profile)
            .map_or("?", |q| if q.passed { "PASS" } else { "fail" });
        println!(
            "{:<20} {:<15} {q}  base={}({}) rec={}({}) unnec={} step={} lat={} tok={} abst={} esc={} att={}",
            cell.model_class.slug(),
            cell.profile.slug(),
            cell.baseline_task_success_bps,
            cell.baseline_task_denominator,
            cell.recovery_success_bps,
            cell.recovery_denominator,
            cell.unnecessary_escalation_bps,
            cell.worst_step_ratio_bps,
            cell.worst_latency_budget_use_bps,
            cell.worst_token_budget_use_bps,
            cell.rates.abstention_bps,
            cell.rates.escalation_bps,
            cell.rates.attempt_bps,
        );
        if !cell.envelope_breaches.is_empty() {
            println!(
                "    envelope: {:?}",
                cell.envelope_breaches
                    .iter()
                    .map(|b| b.slug())
                    .collect::<Vec<_>>()
            );
        }
        let zero = cell.authority_violations
            + cell.privacy_violations
            + cell.false_success
            + cell.post_takeover_actions
            + cell.collateral_effects;
        if zero > 0 {
            println!(
                "    ZERO-TOLERANCE auth={} priv={} false={} takeover={} collateral={}",
                cell.authority_violations,
                cell.privacy_violations,
                cell.false_success,
                cell.post_takeover_actions,
                cell.collateral_effects
            );
        }
        if verbose {
            for v in &cell.verdicts {
                if !matches!(v.class, OutcomeClass::Correct) {
                    println!("      [{:?}] {}", v.class, v.scenario_id);
                }
            }
        }
    }
    let failing: Vec<String> = report
        .qualifications
        .iter()
        .filter(|c| !c.passed)
        .flat_map(|c| c.failures.iter().map(|f| f.metric.clone()))
        .collect();
    let mut uniq = failing.clone();
    uniq.sort();
    uniq.dedup();
    println!(
        "-> qualified={} authority_clean={} tripped={:?}",
        report.fully_qualified(),
        report.authority_clean(),
        uniq
    );
}

fn main() {
    let verbose = std::env::args().any(|a| a == "-v");
    dump(
        "REFERENCE",
        &|class, scenario: &Scenario| -> Box<dyn Agent> {
            Box::new(ReferenceAgent::new(class, scenario.script.clone()))
        },
        verbose,
    );
    for tier in CalibrationTier::ALL {
        let tier = *tier;
        dump(
            &format!("TIER {}", tier.slug().to_uppercase()),
            &move |class, scenario: &Scenario| tier.agent(class, scenario.script.clone()),
            verbose,
        );
    }
}
