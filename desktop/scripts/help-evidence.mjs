/**
 * Capture Help's rendered states and audit them for accessibility.
 *
 * Runs the built evidence harness in Chromium at a desktop and a narrow width,
 * screenshots each of the six states, and runs axe-core against every one.
 * Nothing here contacts a provider or reads private data: the harness serves
 * the synthetic fixture corpus, which is invented content by construction.
 *
 * Usage:
 *   npx vite build --config evidence/vite.config.ts
 *   node scripts/help-evidence.mjs
 */

import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import axe from "axe-core";

const axeSource = axe.source;

const here = dirname(fileURLToPath(import.meta.url));
const desktop = resolve(here, "..");
const distDir = join(desktop, "evidence-dist");
const outDir = join(desktop, "evidence-out");
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const digestFile = (file) => `sha256:${sha256(readFileSync(file))}`;
const evidenceAssets = readdirSync(join(distDir, "assets"))
  .filter((name) => name.endsWith(".js") || name.endsWith(".css"))
  .sort()
  .map((name) => ({ name, digest: digestFile(join(distDir, "assets", name)) }));
if (evidenceAssets.length === 0) throw new Error("built evidence harness has no JS/CSS assets");
const evidenceBundleDigest = `sha256:${sha256(Buffer.from(JSON.stringify(evidenceAssets)))}`;
const publicCorpus = JSON.parse(
  readFileSync(join(desktop, "src/lib/help/canonical/help-corpus-public.v1.json"), "utf8"),
);

/**
 * Serve the built harness over loopback HTTP.
 *
 * ES modules are blocked over `file://` by the same-origin policy, so the
 * harness has to be served. This is a few lines of `node:http` rather than a
 * dependency, and it binds to 127.0.0.1 on an ephemeral port: nothing about
 * this run is reachable from outside the machine.
 */
const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
};
const server = createServer((request, response) => {
  const path = new URL(request.url, "http://127.0.0.1").pathname;
  const file = join(distDir, path === "/" ? "index.html" : path);
  // Never serve outside the built harness, even from a loopback server.
  if (!file.startsWith(distDir)) {
    response.writeHead(403).end();
    return;
  }
  try {
    const body = readFileSync(file);
    const extension = file.slice(file.lastIndexOf("."));
    response.writeHead(200, { "content-type": MIME[extension] ?? "application/octet-stream" });
    response.end(body);
  } catch {
    response.writeHead(404).end();
  }
});
await new Promise((ready) => server.listen(0, "127.0.0.1", ready));
const pageUrl = `http://127.0.0.1:${server.address().port}/index.html`;

const STATES = ["browse", "answer", "ambiguous", "low-confidence", "no-match", "rejected"];
const VIEWPORTS = [
  { name: "desktop", width: 1440, height: 900 },
  { name: "narrow", width: 390, height: 844 },
];

/** Rules that cannot apply to a harness rendering one dialog on a bare page. */
const HARNESS_EXEMPT = new Set([
  // The harness root is the dialog itself; there is no surrounding landmark
  // structure to place it in, and the real app supplies one.
  "region",
  "landmark-one-main",
  "page-has-heading-one",
]);

mkdirSync(outDir, { recursive: true });

// This environment provisions Chromium under PLAYWRIGHT_BROWSERS_PATH at a
// build the installed Playwright does not expect, so the binary is named
// explicitly rather than downloaded. HELP_EVIDENCE_CHROMIUM overrides it.
const executablePath =
  process.env.HELP_EVIDENCE_CHROMIUM ??
  (process.env.PLAYWRIGHT_BROWSERS_PATH
    ? join(process.env.PLAYWRIGHT_BROWSERS_PATH, "chromium")
    : undefined);
const browser = await chromium.launch(executablePath ? { executablePath } : {});
const report = [];
let violations = 0;

for (const viewport of VIEWPORTS) {
  const context = await browser.newContext({
    viewport: { width: viewport.width, height: viewport.height },
    deviceScaleFactor: 1,
    reducedMotion: "reduce",
  });
  for (const state of STATES) {
    const page = await context.newPage();
    await page.goto(`${pageUrl}?state=${state}`);
    // Wait for the dialog to report the state this capture is *for*. Waiting
    // on a frame count instead raced React's re-render and silently produced
    // six identical screenshots of the browse state.
    await page.waitForSelector(`[role='dialog'][data-help-status='${state}']`, {
      timeout: 15_000,
    });

    const shot = join(outDir, `help-${state}-${viewport.name}.png`);
    await page.screenshot({ path: shot, fullPage: false });

    await page.addScriptTag({ content: axeSource });
    const result = await page.evaluate(async () =>
      // eslint-disable-next-line no-undef
      await window.axe.run(document, {
        resultTypes: ["violations"],
        runOnly: { type: "tag", values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"] },
      }),
    );
    const applicable = result.violations.filter((entry) => !HARNESS_EXEMPT.has(entry.id));
    violations += applicable.length;
    report.push({
      state,
      viewport: viewport.name,
      screenshot: `evidence-out/help-${state}-${viewport.name}.png`,
      screenshotDigest: digestFile(shot),
      violations: applicable.map((entry) => ({
        id: entry.id,
        impact: entry.impact,
        help: entry.help,
        nodes: entry.nodes.length,
      })),
    });
    await page.close();
  }
  await context.close();
}

await browser.close();
await new Promise((closed) => server.close(closed));

writeFileSync(
  join(outDir, "accessibility-report.json"),
  `${JSON.stringify(
    {
      generatedFrom: "synthetic fixture corpus",
      publicCorpusDigest: publicCorpus.digest,
      evidenceBundleDigest,
      evidenceAssets,
      report,
    },
    null,
    2,
  )}\n`,
);

for (const entry of report) {
  const label = `${entry.state} @ ${entry.viewport}`;
  if (entry.violations.length === 0) {
    console.log(`ok    ${label} — no WCAG 2.1 A/AA violations`);
  } else {
    console.error(`FAIL  ${label}`);
    for (const violation of entry.violations) {
      console.error(`        ${violation.id} (${violation.impact}) x${violation.nodes}`);
    }
  }
}

if (violations > 0) {
  console.error(`${violations} accessibility violation(s).`);
  process.exit(1);
}
console.log(`Help evidence: ${report.length} captures, 0 violations.`);
