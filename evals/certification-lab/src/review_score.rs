//! Deterministic one-to-one scorer for the code-review benchmark.
//!
//! The review runtime never sees [`crate::review_manifest::HiddenOracleSet`].
//! This module is the only consumer of hidden file/symbol/region/category/atom
//! ground truth.

use std::collections::{BTreeMap, HashSet};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::review_manifest::{
    digest, scorer_digest, ArmId, HiddenOracleSet, IssueCategory, LineRegion, LiveThresholds,
    ScoredLure, ScoredOracle, Severity, UsefulnessFacts, EXPECTED_FAMILY_COUNT,
};

pub const CONFIDENCE_BINS: usize = 10;
pub const BOOTSTRAP_SAMPLES: usize = 1_000;
pub const BOOTSTRAP_SEED: u64 = 0xC0DE_5EED_0001_70B1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoredFinding {
    pub case_id: String,
    pub family_id: String,
    pub opaque_file_id: String,
    pub opaque_symbol_id: String,
    pub region: LineRegion,
    pub category: IssueCategory,
    pub causal_atom: String,
    pub severity: Severity,
    pub confidence_millis: u32,
    pub usefulness: UsefulnessFacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MatchKind {
    TruePositive,
    FalsePositive,
    Duplicate,
    Lure,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArmScore {
    pub arm: ArmId,
    pub true_positives: u32,
    pub false_positives: u32,
    pub false_negatives: u32,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub weighted_precision: f64,
    pub weighted_recall: f64,
    pub weighted_f1: f64,
    pub high_critical_recall: f64,
    pub usefulness: f64,
    pub brier: f64,
    pub ece: f64,
    pub completeness_brier: f64,
    pub weighted_utility: f64,
    pub family_utility: BTreeMap<String, f64>,
    pub family_recall: BTreeMap<String, f64>,
    pub match_digest: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PairedScore {
    pub baseline: ArmScore,
    pub grokptah: ArmScore,
    pub precision_delta: f64,
    pub weighted_recall_delta: f64,
    pub weighted_utility_lift: f64,
    pub weighted_utility_lift_ci_lower: f64,
    pub recall_lift: f64,
    pub recall_lift_ci_lower: f64,
    pub family_wins: u32,
    pub family_count: u32,
    pub worst_family_delta: f64,
    pub token_ratio: f64,
    pub request_ratio: f64,
    pub wall_ratio: f64,
    pub utility_gain_per_10k_tokens: f64,
    pub efficiency_superiority_claimable: bool,
    pub live_thresholds_met: bool,
    pub scorer_digest: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArmCost {
    pub requests: u32,
    pub authoritative_tokens: u64,
    pub wall_millis: u64,
}

pub fn score_paired(
    oracles: &HiddenOracleSet,
    baseline: &[ScoredFinding],
    grokptah: &[ScoredFinding],
    baseline_cost: ArmCost,
    grokptah_cost: ArmCost,
    thresholds: &LiveThresholds,
) -> Result<PairedScore> {
    if oracles.issues.is_empty() {
        bail!("scorer requires hidden oracles");
    }
    let baseline_score = score_arm(ArmId::Baseline, oracles, baseline)?;
    let grokptah_score = score_arm(ArmId::Grokptah, oracles, grokptah)?;
    let family_ids: Vec<String> = grokptah_score.family_utility.keys().cloned().collect();
    if family_ids.len() != EXPECTED_FAMILY_COUNT {
        bail!("scorer family cardinality is not eight");
    }
    let mut wins = 0u32;
    let mut worst = f64::INFINITY;
    let mut lifts = Vec::with_capacity(family_ids.len());
    let mut recall_lifts = Vec::with_capacity(family_ids.len());
    for family in &family_ids {
        let grok = *grokptah_score
            .family_utility
            .get(family)
            .context("family utility")?;
        let base = *baseline_score
            .family_utility
            .get(family)
            .context("family utility")?;
        let delta = grok - base;
        lifts.push(delta);
        if delta > 0.0 {
            wins += 1;
        }
        worst = worst.min(delta);
        let grok_r = *grokptah_score
            .family_recall
            .get(family)
            .context("family recall")?;
        let base_r = *baseline_score
            .family_recall
            .get(family)
            .context("family recall")?;
        recall_lifts.push(grok_r - base_r);
    }
    let utility_lift = grokptah_score.weighted_utility - baseline_score.weighted_utility;
    let recall_lift = grokptah_score.weighted_recall - baseline_score.weighted_recall;
    let utility_ci = cluster_bootstrap_lower(&lifts, BOOTSTRAP_SEED)?;
    let recall_ci = cluster_bootstrap_lower(&recall_lifts, BOOTSTRAP_SEED ^ 1)?;
    let token_ratio = ratio(
        grokptah_cost.authoritative_tokens,
        baseline_cost.authoritative_tokens,
    );
    let request_ratio = ratio(
        u64::from(grokptah_cost.requests),
        u64::from(baseline_cost.requests),
    );
    let wall_ratio = ratio(grokptah_cost.wall_millis, baseline_cost.wall_millis.max(1));
    let extra_tokens = grokptah_cost
        .authoritative_tokens
        .saturating_sub(baseline_cost.authoritative_tokens);
    let utility_gain_per_10k_tokens = if extra_tokens == 0 {
        0.0
    } else {
        utility_lift / (extra_tokens as f64 / 10_000.0)
    };
    let live_thresholds_met = grokptah_score.precision + f64::EPSILON >= thresholds.precision
        && grokptah_score.weighted_recall + f64::EPSILON >= thresholds.weighted_recall
        && grokptah_score.high_critical_recall + f64::EPSILON >= thresholds.high_critical_recall
        && grokptah_score.usefulness + f64::EPSILON >= thresholds.usefulness
        && grokptah_score.brier - f64::EPSILON <= thresholds.brier
        && grokptah_score.ece - f64::EPSILON <= thresholds.ece
        && utility_lift + f64::EPSILON >= thresholds.paired_weighted_utility_lift
        && utility_ci > thresholds.paired_weighted_utility_lift_ci_lower
        && recall_lift + f64::EPSILON >= thresholds.recall_lift
        && recall_ci > thresholds.recall_lift_ci_lower
        && wins >= thresholds.family_wins
        && family_ids.len() as u32 == thresholds.family_count
        && worst > -thresholds.family_max_worse
        && token_ratio - f64::EPSILON <= thresholds.token_ratio
        && request_ratio - f64::EPSILON <= thresholds.request_ratio
        && wall_ratio - f64::EPSILON <= thresholds.wall_ratio;
    Ok(PairedScore {
        precision_delta: grokptah_score.precision - baseline_score.precision,
        weighted_recall_delta: grokptah_score.weighted_recall - baseline_score.weighted_recall,
        weighted_utility_lift: round6(utility_lift),
        weighted_utility_lift_ci_lower: round6(utility_ci),
        recall_lift: round6(recall_lift),
        recall_lift_ci_lower: round6(recall_ci),
        family_wins: wins,
        family_count: family_ids.len() as u32,
        worst_family_delta: round6(worst),
        token_ratio: round6(token_ratio),
        request_ratio: round6(request_ratio),
        wall_ratio: round6(wall_ratio),
        utility_gain_per_10k_tokens: round6(utility_gain_per_10k_tokens),
        efficiency_superiority_claimable: false,
        live_thresholds_met,
        scorer_digest: scorer_digest(),
        baseline: baseline_score,
        grokptah: grokptah_score,
    })
}

pub fn score_arm(
    arm: ArmId,
    oracles: &HiddenOracleSet,
    findings: &[ScoredFinding],
) -> Result<ArmScore> {
    let mut unused_oracles: HashSet<String> = oracles
        .issues
        .iter()
        .map(|issue| issue.issue_id.clone())
        .collect();
    let mut tp = 0u32;
    let mut fp = 0u32;
    let mut tp_weight = 0u32;
    let mut fp_weight = 0u32;
    let mut hc_tp = 0u32;
    let mut hc_total = 0u32;
    let mut usefulness_num = 0u32;
    let mut usefulness_den = 0u32;
    let mut brier_sum = 0.0;
    let mut bins = [(0u32, 0.0, 0u32); CONFIDENCE_BINS];
    let mut family_tp: BTreeMap<String, u32> = BTreeMap::new();
    let mut family_fn: BTreeMap<String, u32> = BTreeMap::new();
    let mut family_oracle: BTreeMap<String, u32> = BTreeMap::new();
    let mut family_useful: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let mut family_tp_w: BTreeMap<String, u32> = BTreeMap::new();
    let mut family_fp_w: BTreeMap<String, u32> = BTreeMap::new();
    let mut family_oracle_w: BTreeMap<String, u32> = BTreeMap::new();
    let mut case_actual: BTreeMap<String, f64> = BTreeMap::new();
    let mut case_predicted: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut match_hasher = Sha256::new();
    for issue in &oracles.issues {
        *family_oracle.entry(issue.family_id.clone()).or_insert(0) += 1;
        *family_oracle_w.entry(issue.family_id.clone()).or_insert(0) += issue.severity.weight();
        if issue.severity.high_or_critical() {
            hc_total += 1;
        }
        case_actual.insert(issue.case_id.clone(), 0.0);
    }
    for finding in findings {
        if finding.confidence_millis > 1_000 {
            bail!("finding confidence is outside 0..=1000 millis");
        }
        validate_region(finding.region)?;
        case_predicted
            .entry(finding.case_id.clone())
            .or_default()
            .push(finding.confidence_millis as f64 / 1_000.0);
        let kind = classify_finding(finding, oracles, &mut unused_oracles);
        match_hasher.update(finding.case_id.as_bytes());
        match_hasher.update([kind as u8]);
        let predicted = finding.confidence_millis as f64 / 1_000.0;
        let outcome = match kind {
            MatchKind::TruePositive => 1.0,
            MatchKind::FalsePositive | MatchKind::Duplicate | MatchKind::Lure => 0.0,
        };
        brier_sum += (predicted - outcome).powi(2);
        let bin = ((finding.confidence_millis as usize) / 100).min(CONFIDENCE_BINS - 1);
        bins[bin].0 += 1;
        bins[bin].1 += predicted;
        if outcome > 0.5 {
            bins[bin].2 += 1;
        }
        match kind {
            MatchKind::TruePositive => {
                tp += 1;
                tp_weight += finding.severity.weight();
                *family_tp.entry(finding.family_id.clone()).or_insert(0) += 1;
                *family_tp_w.entry(finding.family_id.clone()).or_insert(0) +=
                    finding.severity.weight();
                if finding.severity.high_or_critical() {
                    hc_tp += 1;
                }
                let facts =
                    usefulness_overlap(finding.usefulness, required_facts(finding, oracles)?);
                usefulness_num += facts;
                usefulness_den += 4;
                let entry = family_useful
                    .entry(finding.family_id.clone())
                    .or_insert((0, 0));
                entry.0 += facts;
                entry.1 += 4;
                if let Some(actual) = case_actual.get_mut(&finding.case_id) {
                    *actual = 1.0;
                }
            }
            MatchKind::FalsePositive | MatchKind::Duplicate | MatchKind::Lure => {
                fp += 1;
                fp_weight += finding.severity.weight();
                *family_fp_w.entry(finding.family_id.clone()).or_insert(0) +=
                    finding.severity.weight();
            }
        }
    }
    let fn_count = u32::try_from(unused_oracles.len()).context("fn overflow")?;
    for issue in &oracles.issues {
        if unused_oracles.contains(&issue.issue_id) {
            *family_fn.entry(issue.family_id.clone()).or_insert(0) += 1;
        }
    }
    let finding_count = findings.len() as f64;
    let brier = if finding_count == 0.0 {
        1.0
    } else {
        brier_sum / finding_count
    };
    let mut ece = 0.0;
    if finding_count > 0.0 {
        for (count, conf_sum, pos) in bins {
            if count == 0 {
                continue;
            }
            let acc = f64::from(pos) / f64::from(count);
            let conf = conf_sum / f64::from(count);
            ece += (f64::from(count) / finding_count) * (acc - conf).abs();
        }
    } else {
        ece = 1.0;
    }
    let mut completeness = 0.0;
    let cases = case_actual.len().max(1) as f64;
    for (case_id, actual) in &case_actual {
        let predicted = case_predicted
            .get(case_id)
            .map(|values| values.iter().sum::<f64>() / values.len() as f64)
            .unwrap_or(0.0);
        completeness += (predicted - actual).powi(2);
    }
    let completeness_brier = completeness / cases;
    let precision = div(tp, tp + fp);
    let recall = div(tp, tp + fn_count);
    let weighted_precision = div(tp_weight, tp_weight + fp_weight);
    let oracle_weight: u32 = oracles
        .issues
        .iter()
        .map(|issue| issue.severity.weight())
        .sum();
    let weighted_recall = div(tp_weight, oracle_weight);
    let usefulness = if usefulness_den == 0 {
        0.0
    } else {
        f64::from(usefulness_num) / f64::from(usefulness_den)
    };
    let high_critical_recall = div(hc_tp, hc_total);
    let mut family_utility = BTreeMap::new();
    let mut family_recall_map = BTreeMap::new();
    for family in family_oracle.keys() {
        let ftp = *family_tp.get(family).unwrap_or(&0);
        let ffn = *family_fn.get(family).unwrap_or(&0);
        let ftpw = *family_tp_w.get(family).unwrap_or(&0);
        let ffpw = *family_fp_w.get(family).unwrap_or(&0);
        let forw = *family_oracle_w.get(family).unwrap_or(&0);
        let f_prec = div(ftpw, ftpw + ffpw);
        let f_rec = div(ftpw, forw);
        let f_f1 = f1(f_prec, f_rec);
        let (unum, uden) = family_useful.get(family).copied().unwrap_or((0, 0));
        let fuse = if uden == 0 {
            0.0
        } else {
            f64::from(unum) / f64::from(uden)
        };
        family_utility.insert(family.clone(), round6(f_f1 * fuse));
        family_recall_map.insert(family.clone(), round6(div(ftp, ftp + ffn)));
    }
    let weighted_utility = round6(f1(weighted_precision, weighted_recall) * usefulness);
    Ok(ArmScore {
        arm,
        true_positives: tp,
        false_positives: fp,
        false_negatives: fn_count,
        precision: round6(precision),
        recall: round6(recall),
        f1: round6(f1(precision, recall)),
        weighted_precision: round6(weighted_precision),
        weighted_recall: round6(weighted_recall),
        weighted_f1: round6(f1(weighted_precision, weighted_recall)),
        high_critical_recall: round6(high_critical_recall),
        usefulness: round6(usefulness),
        brier: round6(brier),
        ece: round6(ece),
        completeness_brier: round6(completeness_brier),
        weighted_utility,
        family_utility,
        family_recall: family_recall_map,
        match_digest: format!("{:x}", match_hasher.finalize()),
    })
}

fn classify_finding(
    finding: &ScoredFinding,
    oracles: &HiddenOracleSet,
    unused_oracles: &mut HashSet<String>,
) -> MatchKind {
    if oracles.lures.iter().any(|lure| lure_match(finding, lure)) {
        return MatchKind::Lure;
    }
    if let Some(issue) = oracles
        .issues
        .iter()
        .find(|issue| oracle_match(finding, issue))
    {
        if unused_oracles.remove(&issue.issue_id) {
            MatchKind::TruePositive
        } else {
            MatchKind::Duplicate
        }
    } else {
        MatchKind::FalsePositive
    }
}

fn oracle_match(finding: &ScoredFinding, issue: &ScoredOracle) -> bool {
    finding.case_id == issue.case_id
        && finding.opaque_file_id == issue.opaque_file_id
        && finding.opaque_symbol_id == issue.opaque_symbol_id
        && finding.category == issue.category
        && finding.causal_atom == issue.causal_atom
        && region_contained(finding.region, issue.accepted_region)
}

fn lure_match(finding: &ScoredFinding, lure: &ScoredLure) -> bool {
    finding.case_id == lure.case_id
        && finding.opaque_file_id == lure.opaque_file_id
        && finding.opaque_symbol_id == lure.opaque_symbol_id
        && finding.category == lure.category
        && finding.causal_atom == lure.causal_atom
        && finding.region == lure.region
}

fn region_contained(inner: LineRegion, outer: LineRegion) -> bool {
    inner.start_line >= outer.start_line && inner.end_line <= outer.end_line
}

fn required_facts(finding: &ScoredFinding, oracles: &HiddenOracleSet) -> Result<UsefulnessFacts> {
    oracles
        .issues
        .iter()
        .find(|issue| oracle_match(finding, issue))
        .map(|issue| issue.usefulness_facts)
        .context("true-positive finding lost its oracle")
}

fn usefulness_overlap(found: UsefulnessFacts, required: UsefulnessFacts) -> u32 {
    let pairs = [
        (found.cause, required.cause),
        (found.impact, required.impact),
        (found.remediation, required.remediation),
        (found.test, required.test),
    ];
    pairs
        .into_iter()
        .filter(|(found, required)| *required && *found)
        .count() as u32
}

fn validate_region(region: LineRegion) -> Result<()> {
    if region.start_line == 0 || region.end_line == 0 || region.start_line > region.end_line {
        bail!("finding region is invalid");
    }
    Ok(())
}

fn div(num: u32, den: u32) -> f64 {
    if den == 0 {
        0.0
    } else {
        f64::from(num) / f64::from(den)
    }
}

fn f1(precision: f64, recall: f64) -> f64 {
    if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    }
}

fn ratio(num: u64, den: u64) -> f64 {
    if den == 0 {
        f64::INFINITY
    } else {
        num as f64 / den as f64
    }
}

fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn cluster_bootstrap_lower(family_deltas: &[f64], seed: u64) -> Result<f64> {
    if family_deltas.len() != EXPECTED_FAMILY_COUNT {
        bail!("bootstrap requires eight family clusters");
    }
    let mut rng = SplitMix64(seed);
    let mut samples = Vec::with_capacity(BOOTSTRAP_SAMPLES);
    for _ in 0..BOOTSTRAP_SAMPLES {
        let mut total = 0.0;
        for _ in 0..family_deltas.len() {
            let index = rng.next_u64() as usize % family_deltas.len();
            total += family_deltas[index];
        }
        samples.push(total / family_deltas.len() as f64);
    }
    samples.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((BOOTSTRAP_SAMPLES as f64) * 0.025).floor() as usize;
    Ok(round6(samples[idx.min(samples.len() - 1)]))
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

pub fn findings_digest(findings: &[ScoredFinding]) -> String {
    let mut hasher = Sha256::new();
    for finding in findings {
        hasher.update(finding.case_id.as_bytes());
        hasher.update(finding.opaque_file_id.as_bytes());
        hasher.update(finding.causal_atom.as_bytes());
        hasher.update(finding.confidence_millis.to_le_bytes());
        hasher.update(finding.region.start_line.to_le_bytes());
        hasher.update(finding.region.end_line.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub fn oracles_digest(oracles: &HiddenOracleSet) -> String {
    let mut hasher = Sha256::new();
    for issue in &oracles.issues {
        hasher.update(issue.issue_id.as_bytes());
        hasher.update(issue.opaque_file_id.as_bytes());
        hasher.update(issue.causal_atom.as_bytes());
        hasher.update(issue.accepted_region.start_line.to_le_bytes());
        hasher.update(issue.accepted_region.end_line.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub fn expected_match_locked(
    oracles: &HiddenOracleSet,
    findings: &[ScoredFinding],
) -> Result<String> {
    let scored = score_arm(ArmId::Grokptah, oracles, findings)?;
    Ok(digest(
        format!(
            "{}:{}:{}:{}",
            scored.true_positives,
            scored.false_positives,
            scored.false_negatives,
            scored.match_digest
        )
        .as_bytes(),
    ))
}
