/**
 * Validate the identity and completeness of one captured Help evidence report.
 *
 * Kept separate from the manifest writer so adversarial tests can challenge
 * the report contract without launching a browser or depending on git state.
 */

export const HELP_EVIDENCE_STATES = Object.freeze([
  "browse",
  "answer",
  "ambiguous",
  "low-confidence",
  "no-match",
  "rejected",
]);

export const HELP_EVIDENCE_VIEWPORTS = Object.freeze(["desktop", "narrow"]);

export const expectedHelpScreenshot = (state, viewport) =>
  `evidence-out/help-${state}-${viewport}.png`;

/**
 * Return the exact screenshot/digest map after validating all report rows.
 *
 * `screenshotDigest` is supplied by the caller so this function remains pure
 * with respect to the filesystem and can be exercised with hostile reports.
 */
export function validateHelpEvidenceReport(report, {
  publicCorpusDigest,
  evidenceBundleDigest,
  evidenceAssets,
  screenshotDigest,
}) {
  if (report.generatedFrom !== "synthetic fixture corpus") {
    throw new Error("accessibility evidence is not labelled synthetic");
  }
  if (report.publicCorpusDigest !== publicCorpusDigest) {
    throw new Error("accessibility evidence is stale for the public corpus");
  }
  if (report.evidenceBundleDigest !== evidenceBundleDigest) {
    throw new Error("accessibility evidence is stale for the built evidence harness");
  }
  if (JSON.stringify(report.evidenceAssets) !== JSON.stringify(evidenceAssets)) {
    throw new Error("accessibility evidence asset set is stale or reordered");
  }
  if (!Array.isArray(report.report)) throw new Error("accessibility report rows are missing");

  const expected = new Set(
    HELP_EVIDENCE_STATES.flatMap((state) =>
      HELP_EVIDENCE_VIEWPORTS.map((viewport) => `${state}:${viewport}`),
    ),
  );
  const seenScreenshots = new Set();
  const seenScreenshotDigests = new Map();
  const screenshots = {};
  for (const entry of report.report) {
    const key = `${entry.state}:${entry.viewport}`;
    if (!expected.delete(key)) throw new Error(`unexpected or duplicate evidence row ${key}`);
    const expectedScreenshot = expectedHelpScreenshot(entry.state, entry.viewport);
    if (entry.screenshot !== expectedScreenshot) {
      throw new Error(
        `evidence row ${key} names ${entry.screenshot}; expected ${expectedScreenshot}`,
      );
    }
    if (!seenScreenshots.add(entry.screenshot)) {
      throw new Error(`duplicate evidence screenshot ${entry.screenshot}`);
    }
    if (!Array.isArray(entry.violations) || entry.violations.length !== 0) {
      throw new Error(`accessibility violations remain for ${key}`);
    }
    const actualDigest = screenshotDigest(entry.screenshot);
    if (entry.screenshotDigest !== actualDigest) {
      throw new Error(`screenshot digest mismatch for ${entry.screenshot}`);
    }
    const reusedBy = seenScreenshotDigests.get(actualDigest);
    if (reusedBy !== undefined) {
      throw new Error(
        `duplicate screenshot bytes for ${entry.screenshot}; digest already used by ${reusedBy}`,
      );
    }
    seenScreenshotDigests.set(actualDigest, entry.screenshot);
    screenshots[entry.screenshot] = actualDigest;
  }
  if (expected.size > 0) {
    throw new Error(`missing evidence rows: ${[...expected].sort().join(", ")}`);
  }
  return screenshots;
}
