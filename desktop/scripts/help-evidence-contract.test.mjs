import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  HELP_EVIDENCE_STATES,
  HELP_EVIDENCE_VIEWPORTS,
  expectedHelpScreenshot,
  validateHelpEvidenceReport,
} from "./help-evidence-contract.mjs";

const assets = [{ name: "index.js", digest: "sha256:asset" }];

function report() {
  return {
    generatedFrom: "synthetic fixture corpus",
    publicCorpusDigest: "sha256:corpus",
    evidenceBundleDigest: "sha256:harness",
    evidenceAssets: assets,
    report: HELP_EVIDENCE_STATES.flatMap((state) =>
      HELP_EVIDENCE_VIEWPORTS.map((viewport) => {
        const screenshot = expectedHelpScreenshot(state, viewport);
        return {
          state,
          viewport,
          screenshot,
          screenshotDigest: `sha256:${screenshot}`,
          violations: [],
        };
      }),
    ),
  };
}

const validate = (candidate) =>
  validateHelpEvidenceReport(candidate, {
    publicCorpusDigest: "sha256:corpus",
    evidenceBundleDigest: "sha256:harness",
    evidenceAssets: assets,
    screenshotDigest: (screenshot) => `sha256:${screenshot}`,
  });

describe("Semantic Help evidence identity", () => {
  it("accepts one exact screenshot for every state and viewport", () => {
    assert.equal(Object.keys(validate(report())).length, 12);
  });

  it("rejects a coherently redigested report that reuses one screenshot", () => {
    const forged = report();
    const reused = forged.report[0].screenshot;
    for (const row of forged.report) {
      row.screenshot = reused;
      row.screenshotDigest = `sha256:${reused}`;
    }
    assert.throws(() => validate(forged), /names .*; expected evidence-out\/help-/);
  });

  it("rejects missing, duplicated, and digest-tampered rows", () => {
    const missing = report();
    missing.report.pop();
    assert.throws(() => validate(missing), /missing evidence rows/);

    const duplicated = report();
    duplicated.report[1] = { ...duplicated.report[0] };
    assert.throws(() => validate(duplicated), /duplicate evidence row/);

    const tampered = report();
    tampered.report[0].screenshotDigest = "sha256:tampered";
    assert.throws(() => validate(tampered), /screenshot digest mismatch/);
  });
});
