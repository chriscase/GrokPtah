//! Report rendering.
//!
//! Two forms, one source: canonical JSON for machines and a Markdown summary
//! for people. Both are byte-stable given the same suite result, so a report
//! can be committed and diffed, and a change in the numbers shows up as a
//! change in the file rather than as something a reader has to notice.
//!
//! The Markdown deliberately leads with the authority line rather than the
//! task line. A build that scores well on coverage and breaches an authority
//! threshold has not "mostly passed".

use std::fmt::Write as _;

use crate::matrix::{LaneCoverage, WorkflowMatrix};
use crate::modelclass::BPS_FULL;
use crate::scoring::{CellQualification, CellScore};
use crate::suite::SuiteReport;

/// Render basis points as a percentage with two decimals, without floats.
#[must_use]
pub fn pct(value: u32) -> String {
    let whole = value / 100;
    let frac = value % 100;
    format!("{whole}.{frac:02}%")
}

/// The canonical JSON form of a suite report.
#[must_use]
pub fn to_json(report: &SuiteReport) -> String {
    crate::digest::canonical_json_pretty(report)
}

/// The human-readable summary.
#[must_use]
pub fn to_markdown(report: &SuiteReport, matrix: &WorkflowMatrix) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# GrokPtah Computer Use qualification report\n");
    let _ = writeln!(out, "- Schema: `{}`", report.schema_version);
    let _ = writeln!(out, "- Scenarios: {}", report.scenario_count);
    let _ = writeln!(
        out,
        "- Catalog digest: `{}`",
        report.scenario_catalog_digest
    );
    let _ = writeln!(out, "- Suite digest: `{}`", report.suite_digest);
    let _ = writeln!(
        out,
        "- Authority and privacy: **{}**",
        if report.authority_clean() {
            "clean"
        } else {
            "BREACHED"
        }
    );
    let _ = writeln!(
        out,
        "- Full qualification: **{}**\n",
        if report.fully_qualified() {
            "passed"
        } else {
            "not met"
        }
    );

    let _ = writeln!(out, "## Cells\n");
    let _ = writeln!(
        out,
        "| model class | profile | baseline | recovery | unnecessary esc. | abstention | unsafe prop. | evidence | replay | verdict |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|---|");
    for cell in &report.cells {
        let qualification = report
            .qualification(cell.model_class, cell.profile)
            .map_or("?", |q| if q.passed { "pass" } else { "fail" });
        let _ = writeln!(
            out,
            "| {} | {} | {} (n={}) | {} (n={}) | {} | {} (n={}) | {} | {} | {} | {} |",
            cell.model_class.slug(),
            cell.profile.slug(),
            pct(cell.baseline_task_success_bps),
            cell.baseline_task_denominator,
            pct(cell.recovery_success_bps),
            cell.recovery_denominator,
            pct(cell.unnecessary_escalation_bps),
            pct(cell.abstention_quality_bps),
            cell.abstention_denominator,
            pct(cell.unsafe_proposal_bps),
            pct(cell.evidence_completeness_bps),
            pct(cell.deterministic_replay_bps),
            qualification,
        );
    }

    let _ = writeln!(out, "\n## Bounded efficiency envelope\n");
    let _ = writeln!(
        out,
        "Stopping rates are measured over the scenarios where finishing was the \
         right answer; the attempt floor is measured over the whole catalog.\n"
    );
    let _ = writeln!(
        out,
        "| model class | profile | abstained | escalated | attempted | breaches |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|");
    for cell in &report.cells {
        let breaches = if cell.envelope_breaches.is_empty() {
            "none".to_owned()
        } else {
            cell.envelope_breaches
                .iter()
                .map(|breach| breach.slug())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            cell.model_class.slug(),
            cell.profile.slug(),
            pct(cell.rates.abstention_bps),
            pct(cell.rates.escalation_bps),
            pct(cell.rates.attempt_bps),
            breaches,
        );
    }

    let _ = writeln!(out, "\n## Zero-tolerance counters\n");
    let _ = writeln!(
        out,
        "| model class | profile | authority | privacy | false success | post-takeover | collateral |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|");
    for cell in &report.cells {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} |",
            cell.model_class.slug(),
            cell.profile.slug(),
            cell.authority_violations,
            cell.privacy_violations,
            cell.false_success,
            cell.post_takeover_actions,
            cell.collateral_effects,
        );
    }

    let failing: Vec<&CellQualification> = report
        .qualifications
        .iter()
        .filter(|cell| !cell.passed)
        .collect();
    if failing.is_empty() {
        let _ = writeln!(out, "\nNo threshold was missed in any cell.\n");
    } else {
        let _ = writeln!(out, "\n## Missed thresholds\n");
        for cell in failing {
            let _ = writeln!(
                out,
                "\n### {} / {}\n",
                cell.model_class.slug(),
                cell.profile.slug()
            );
            for failure in &cell.failures {
                let _ = writeln!(
                    out,
                    "- `{}`: observed {}, {} {}{}",
                    failure.metric,
                    failure.observed,
                    if failure.is_floor {
                        "needs at least"
                    } else {
                        "allows at most"
                    },
                    failure.required,
                    if failure.authority_bearing {
                        " **(authority)**"
                    } else {
                        ""
                    },
                );
            }
        }
    }

    let _ = writeln!(out, "\n## Non-correct outcomes\n");
    let mut any = false;
    for cell in &report.cells {
        for verdict in &cell.verdicts {
            if verdict.class.is_correct() {
                continue;
            }
            any = true;
            let _ = writeln!(
                out,
                "- `{}` / `{}` / `{}` -- {:?}",
                cell.model_class.slug(),
                cell.profile.slug(),
                verdict.scenario_id,
                verdict.class,
            );
        }
    }
    if !any {
        let _ = writeln!(
            out,
            "Every scenario landed on its expected outcome in every cell.\n"
        );
    }

    let _ = writeln!(out, "\n## Representative workflow matrix\n");
    let _ = writeln!(out, "| lane | coverage | scenarios | caveat |");
    let _ = writeln!(out, "|---|---|---|---|");
    for row in &matrix.lanes {
        let coverage = match row.coverage {
            LaneCoverage::Covered => "covered",
            LaneCoverage::Partial => "partial",
            LaneCoverage::NotCovered => "not covered",
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            row.lane.slug(),
            coverage,
            row.scenario_ids.len(),
            row.caveat.replace('\n', " "),
        );
    }

    let _ = writeln!(out, "\n## Comparison status\n");
    let _ = writeln!(out, "{}\n", matrix.external_comparison_statement);
    let _ = writeln!(out, "A reproducible head-to-head would require:\n");
    for step in &matrix.head_to_head_protocol {
        let _ = writeln!(out, "1. {step}");
    }

    out
}

/// The calibration evidence table.
///
/// Shows what the reference agent scores against what each named calibration
/// tier scores, so a reader can see for themselves that a threshold sits
/// between the two rather than taking the claim on trust. This is the
/// artifact that answers "is this floor actually doing anything".
#[must_use]
pub fn calibration_markdown(rows: &[CalibrationRow]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Threshold calibration evidence\n");
    let _ = writeln!(
        out,
        "Each row is one subject run across the whole matrix. The reference \
         agent is the competent policy the thresholds are set below; every \
         other row is a synthetic behaviour chosen to isolate one measurement \
         axis.\n"
    );
    let _ = writeln!(
        out,
        "**These are not model simulations.** No row claims that any real \
         model behaves this way. A calibration result means \"this threshold \
         separates the reference from this defined behaviour\", never \"a small \
         model scores X\".\n"
    );
    let _ = writeln!(
        out,
        "| subject | qualified | authority clean | thresholds tripped |"
    );
    let _ = writeln!(out, "|---|---|---|---|");
    for row in rows {
        let tripped = if row.tripped.is_empty() {
            "none".to_owned()
        } else {
            row.tripped.join(", ")
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            row.subject,
            if row.qualified { "yes" } else { "no" },
            if row.authority_clean { "yes" } else { "no" },
            tripped,
        );
    }
    out
}

/// One subject's result in the calibration table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationRow {
    pub subject: String,
    pub qualified: bool,
    pub authority_clean: bool,
    /// Threshold metric names tripped anywhere in the matrix, sorted.
    pub tripped: Vec<String>,
}

impl CalibrationRow {
    /// Summarise one suite run.
    #[must_use]
    pub fn from_report(subject: &str, report: &SuiteReport) -> Self {
        let mut tripped: Vec<String> = report
            .qualifications
            .iter()
            .flat_map(|cell| cell.failures.iter().map(|failure| failure.metric.clone()))
            .collect();
        tripped.sort();
        tripped.dedup();
        Self {
            subject: subject.to_owned(),
            qualified: report.fully_qualified(),
            authority_clean: report.authority_clean(),
            tripped,
        }
    }
}

/// One-line summary suitable for a CI log.
#[must_use]
pub fn one_line(report: &SuiteReport) -> String {
    let cells = report.cells.len();
    let passed = report
        .qualifications
        .iter()
        .filter(|cell| cell.passed)
        .count();
    format!(
        "cu-bench: {passed}/{cells} cells qualified, authority {}, suite {}",
        if report.authority_clean() {
            "clean"
        } else {
            "BREACHED"
        },
        &report.suite_digest[..16],
    )
}

/// Worst observed value of a metric across cells, for a quick read.
#[must_use]
pub fn worst_authority_margin(cells: &[CellScore]) -> u32 {
    cells
        .iter()
        .map(|cell| {
            cell.abstention_quality_bps
                .min(cell.evidence_completeness_bps)
        })
        .min()
        .unwrap_or(BPS_FULL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog, matrix, suite};

    fn report() -> SuiteReport {
        let scenarios = catalog::all();
        let factory = suite::reference_factory();
        suite::run_matrix(&scenarios, &factory)
    }

    #[test]
    fn percentages_avoid_floating_point() {
        assert_eq!(pct(10_000), "100.00%");
        assert_eq!(pct(9_230), "92.30%");
        assert_eq!(pct(5), "0.05%");
        assert_eq!(pct(0), "0.00%");
    }

    #[test]
    fn the_markdown_report_is_stable_across_renders() {
        let report = report();
        let matrix = matrix::workflow_matrix();
        assert_eq!(to_markdown(&report, &matrix), to_markdown(&report, &matrix));
    }

    #[test]
    fn the_report_leads_with_authority_not_coverage() {
        let report = report();
        let markdown = to_markdown(&report, &matrix::workflow_matrix());
        let authority = markdown
            .find("Authority and privacy")
            .expect("authority line");
        let cells = markdown.find("## Cells").expect("cells section");
        assert!(authority < cells);
    }

    #[test]
    fn the_report_carries_the_no_comparison_statement() {
        let markdown = to_markdown(&report(), &matrix::workflow_matrix());
        assert!(markdown.contains("no comparative claim"));
    }

    #[test]
    fn the_json_report_is_canonical_and_stable() {
        let report = report();
        assert_eq!(to_json(&report), to_json(&report));
    }

    #[test]
    fn the_one_line_summary_names_the_authority_state() {
        let line = one_line(&report());
        assert!(line.contains("authority"));
    }
}
