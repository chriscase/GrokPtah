/**
 * Offline retrieval evaluation reporter.
 *
 *   node --import ./scripts/register-ts-hook.mjs scripts/run-help-eval.mjs
 *
 * Deterministic: no network, no sampling, no clock. Exits non-zero when a
 * threshold in HELP_EVAL_THRESHOLDS is not met.
 */
const { evaluateHelpGoldSet, HELP_EVAL_THRESHOLDS } = await import("../src/lib/help/eval/harness.ts");
const { HELP_CORPUS } = await import("../src/lib/help/canonical/corpus.ts");
const { HELP_MODEL_STATS } = await import("../src/lib/help/model/artifact.ts");
const { HELP_ABSTENTION_THRESHOLD } = await import("../src/lib/help/retrieval/hybrid.ts");

const metrics = evaluateHelpGoldSet();
const pct = (value) => `${(value * 100).toFixed(1)}%`;

console.log("Help retrieval evaluation (offline, deterministic)");
console.log(`  corpus:    ${HELP_CORPUS.digest}`);
console.log(`  model:     ${HELP_MODEL_STATS.modelId} dims=${HELP_MODEL_STATS.dims} vocab=${HELP_MODEL_STATS.vocabularySize}`);
console.log(`  threshold: abstain below fused ${HELP_ABSTENTION_THRESHOLD}`);
console.log("");
console.log(`  queries:   ${metrics.total} (${metrics.answerable} answerable, ${metrics.mustAbstain} must-abstain)`);
console.log("");

const rows = [
  ["Recall@1 (relevant)", metrics.recallAt1, HELP_EVAL_THRESHOLDS.recallAt1, "min"],
  ["Top-1 exact article", metrics.topExactRate, HELP_EVAL_THRESHOLDS.topExactRate, "min"],
  ["Recall@3", metrics.recallAt3, HELP_EVAL_THRESHOLDS.recallAt3, "min"],
  ["MRR", metrics.mrr, HELP_EVAL_THRESHOLDS.mrr, "min"],
  ["Citation accuracy", metrics.citationAccuracy, HELP_EVAL_THRESHOLDS.citationAccuracy, "min"],
  ["False-answer rate", metrics.falseAnswerRate, HELP_EVAL_THRESHOLDS.falseAnswerRate, "max"],
  ["Answerable abstention", metrics.answerableAbstentionRate, HELP_EVAL_THRESHOLDS.answerableAbstentionRate, "max"],
];
let failed = 0;
for (const [name, value, threshold, direction] of rows) {
  const ok = direction === "min" ? value >= threshold : value <= threshold;
  if (!ok) failed += 1;
  console.log(
    `  ${ok ? "PASS" : "FAIL"}  ${name.padEnd(22)} ${pct(value).padStart(7)}  (${direction} ${pct(threshold)})`,
  );
}
console.log(`\n  Abstention recall on unsupported: ${pct(metrics.abstentionRecall)}`);

console.log("\n  Per category (answerable only):");
for (const [category, stats] of Object.entries(metrics.perCategory).sort()) {
  if (stats.count === 0) continue;
  console.log(
    `    ${category.padEnd(14)} n=${String(stats.count).padStart(3)}  R@1 ${pct(stats.recallAt1).padStart(7)}  R@3 ${pct(stats.recallAt3).padStart(7)}`,
  );
}

const misses = metrics.outcomes.filter(
  (outcome) => outcome.query.expectedArticleId !== null && !outcome.exactTop,
);
if (misses.length > 0) {
  console.log(`\n  Rank-1 misses (${misses.length}):`);
  for (const miss of misses) {
    console.log(
      `    [${miss.query.id}] "${miss.query.query}"\n` +
        `        expected ${miss.query.expectedArticleId}\n` +
        `        got      ${miss.topArticleId ?? `(abstained, confidence ${miss.confidence.toFixed(3)})`}` +
        `${miss.rankOfRelevant ? ` — relevant at rank ${miss.rankOfRelevant}` : ""}`,
    );
  }
}
const falseAnswers = metrics.outcomes.filter((outcome) => outcome.falseAnswer);
if (falseAnswers.length > 0) {
  console.log(`\n  False answers (${falseAnswers.length}):`);
  for (const entry of falseAnswers) {
    console.log(`    [${entry.query.id}] "${entry.query.query}" -> ${entry.topArticleId} (${entry.confidence.toFixed(3)})`);
  }
}
const badCitations = metrics.outcomes.filter(
  (outcome) => outcome.query.expectedArticleId !== null && !outcome.abstained && !outcome.citationOk,
);
if (badCitations.length > 0) {
  console.log(`\n  Citation failures (${badCitations.length}):`);
  for (const entry of badCitations) {
    console.log(`    [${entry.query.id}] ${entry.citationDetail}`);
  }
}

if (failed > 0) {
  console.error(`\n${failed} metric(s) below threshold`);
  process.exit(1);
}
console.log("\nAll retrieval metrics met their thresholds.");

// ---- claim-bound citation coverage -----------------------------------------
const { evaluateHelpCitationBinding, HELP_CITATION_THRESHOLDS } = await import(
  "../src/lib/help/eval/citations.ts"
);
const citations = evaluateHelpCitationBinding();
console.log(
  `\nClaim-bound citations: ${citations.evaluated} answers built from gold queries, ` +
    `${citations.swapEvaluated} with a disjoint chunk to swap in`,
);
let citationFailures = 0;
for (const [label, actual, min] of [
  ["Faithful answer accepted", citations.faithfulAcceptanceRate, HELP_CITATION_THRESHOLDS.faithfulAcceptanceRate],
  ["Swapped citation rejected", citations.swapRejectionRate, HELP_CITATION_THRESHOLDS.swapRejectionRate],
]) {
  const ok = actual >= min;
  if (!ok) citationFailures += 1;
  console.log(
    `  ${ok ? "PASS" : "FAIL"}  ${label.padEnd(26)}${(actual * 100).toFixed(1)}%  (min ${(min * 100).toFixed(1)}%)`,
  );
}
for (const entry of citations.cases.filter((c) => !c.faithfulAccepted).slice(0, 10)) {
  console.log(`    [${entry.query.id}] faithful answer refused: ${entry.faithfulDetail}`);
}
for (const entry of citations.cases.filter((c) => c.swapRejected === false)) {
  console.log(`    [${entry.query.id}] swapped citation accepted: ${entry.swapDetail}`);
}
if (citationFailures > 0) {
  console.error(`\n${citationFailures} citation metric(s) below threshold`);
  process.exit(1);
}
console.log("All citation-binding metrics met their thresholds.");
