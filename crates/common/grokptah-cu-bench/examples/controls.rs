//! Runs the negative-control agents to confirm the harness detects failure.
use grokptah_cu_bench::agent::{Agent, NaiveAgent, StubbornAgent};
use grokptah_cu_bench::modelclass::ModelClass;
use grokptah_cu_bench::profile::ExecutionProfile;
use grokptah_cu_bench::runner::execute;
use grokptah_cu_bench::scenario::NegativeControl;
use grokptah_cu_bench::{catalog, scoring};

fn main() {
    let profile = ExecutionProfile::balanced();
    for scenario in catalog::all() {
        let mut naive: Box<dyn Agent> = Box::new(NaiveAgent::new(
            ModelClass::LargeVision,
            scenario.script.clone(),
        ));
        let record = execute(&scenario, &profile, naive.as_mut());
        let verdict = scoring::classify(&scenario, &record, &profile);
        let auth_refusals = record
            .steps
            .iter()
            .filter(|step| {
                step.decision
                    .as_ref()
                    .is_some_and(|d| d.is_authority_refusal())
            })
            .count();
        println!(
            "{:52} control={:24?} class={:22?} outcome={:?} auth_refusals={} unsafe={} authority_viol={} privacy={}",
            scenario.id,
            scenario.negative_control,
            verdict.class,
            record.outcome,
            auth_refusals,
            record.unsafe_proposals,
            record.authority_violations,
            record.privacy_violations.len(),
        );
        let _ = NegativeControl::NotChecked;
    }

    println!("\n-- stubborn control on the stationarity fixture --");
    let scenario = catalog::by_id("stationarity_loop/refresh_that_never_changes").unwrap();
    let mut stubborn: Box<dyn Agent> =
        Box::new(StubbornAgent::new(ModelClass::LargeVision, "Refresh"));
    let record = execute(&scenario, &profile, stubborn.as_mut());
    println!(
        "outcome={:?} steps={} tokens={}",
        record.outcome,
        record.steps.len(),
        record.total_tokens()
    );
}
