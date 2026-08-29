/**
 * Produce a deterministic provenance manifest for committed Semantic Help evidence.
 *
 * This does not launch a browser. It binds the exact candidate/base commits, corpus bytes,
 * built renderer assets, accessibility report, and every expected screenshot so hosted CI can
 * qualify already-captured synthetic evidence without depending on GUI availability.
 */

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const desktop = resolve(here, "..");
const repository = resolve(desktop, "..");
const evidence = join(desktop, "evidence-out");
const outputIndex = process.argv.indexOf("--out");
const output =
  outputIndex >= 0
    ? resolve(process.argv[outputIndex + 1])
    : join(evidence, "provenance-manifest.json");

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const digestFile = (file) => `sha256:${sha256(readFileSync(file))}`;
const git = (...args) =>
  execFileSync("git", args, { cwd: repository, encoding: "utf8" }).trim();

let event = {};
if (process.env.GITHUB_EVENT_PATH && existsSync(process.env.GITHUB_EVENT_PATH)) {
  event = JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, "utf8"));
}
const checkedOutCommit = git("rev-parse", "HEAD");
const candidateCommit = event.pull_request?.head?.sha ?? checkedOutCommit;
const baseCommit =
  process.env.HELP_EVIDENCE_BASE_SHA ??
  event.pull_request?.base?.sha ??
  git("merge-base", "HEAD", "origin/main");
if (candidateCommit !== checkedOutCommit) {
  throw new Error(
    `candidate/checkout mismatch: candidate=${candidateCommit} checkout=${checkedOutCommit}`,
  );
}
if (!/^[0-9a-f]{40}$/.test(baseCommit)) throw new Error(`invalid base commit ${baseCommit}`);
execFileSync("git", ["cat-file", "-e", `${baseCommit}^{commit}`], { cwd: repository });
execFileSync("git", ["merge-base", "--is-ancestor", baseCommit, candidateCommit], {
  cwd: repository,
});

const publicCorpusFile = join(desktop, "src/lib/help/canonical/help-corpus-public.v1.json");
const fullCorpusFile = join(desktop, "src/lib/help/canonical/help-corpus.v1.json");
const publicCorpus = JSON.parse(readFileSync(publicCorpusFile, "utf8"));
const fullCorpus = JSON.parse(readFileSync(fullCorpusFile, "utf8"));

const assetDir = join(desktop, "dist/assets");
const assets = readdirSync(assetDir)
  .filter((name) => name.endsWith(".js") || name.endsWith(".css"))
  .sort()
  .map((name) => ({ name, digest: digestFile(join(assetDir, name)) }));
if (assets.length === 0) throw new Error("built renderer has no JS/CSS assets");
const bundleDigest = `sha256:${sha256(Buffer.from(JSON.stringify(assets)))}`;

const reportFile = join(evidence, "accessibility-report.json");
const report = JSON.parse(readFileSync(reportFile, "utf8"));
if (report.generatedFrom !== "synthetic fixture corpus") {
  throw new Error("accessibility evidence is not labelled synthetic");
}
const states = ["browse", "answer", "ambiguous", "low-confidence", "no-match", "rejected"];
const viewports = ["desktop", "narrow"];
const expected = new Set(states.flatMap((state) => viewports.map((viewport) => `${state}:${viewport}`)));
const screenshots = {};
for (const entry of report.report) {
  const key = `${entry.state}:${entry.viewport}`;
  if (!expected.delete(key)) throw new Error(`unexpected or duplicate evidence row ${key}`);
  if (entry.violations.length !== 0) throw new Error(`accessibility violations remain for ${key}`);
  const screenshot = join(desktop, entry.screenshot);
  if (!existsSync(screenshot)) throw new Error(`missing evidence screenshot ${entry.screenshot}`);
  screenshots[entry.screenshot] = digestFile(screenshot);
}
if (expected.size > 0) throw new Error(`missing evidence rows: ${[...expected].sort().join(", ")}`);

const manifest = {
  schemaVersion: "grokptah.help-evidence.v1",
  candidateCommit,
  baseCommit,
  checkedOutCommit,
  corpus: {
    schemaVersion: fullCorpus.schema_version,
    contentVersion: fullCorpus.content_version,
    fullDigest: fullCorpus.digest,
    fullArtifactDigest: digestFile(fullCorpusFile),
    publicDigest: publicCorpus.digest,
    publicArtifactDigest: digestFile(publicCorpusFile),
  },
  renderer: { bundleDigest, assets },
  evidence: {
    generatedFrom: report.generatedFrom,
    accessibilityReportDigest: digestFile(reportFile),
    screenshots,
  },
};

writeFileSync(output, `${JSON.stringify(manifest, null, 2)}\n`);
console.log(JSON.stringify(manifest, null, 2));
