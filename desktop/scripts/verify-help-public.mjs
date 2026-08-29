/**
 * Verify what Help is allowed to ship.
 *
 * Run after `npm run build`. The checks below are on artifacts — the emitted
 * corpus files and the built renderer bundle — rather than on source, because
 * the failure mode is a build that carries content the source never meant to
 * expose. Reviewing an import list cannot catch that; reading the bytes can.
 *
 * This is deliberately Help-scoped. There is no published `@grokptah/client`
 * package on this branch, so a check named for one would be claiming a surface
 * that does not exist.
 */

import { readFileSync, readdirSync, existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const desktop = resolve(here, "..");
const helpDir = join(desktop, "src/lib/help");

const failures = [];
const checks = [];

function check(name, run) {
  try {
    run();
    checks.push(name);
  } catch (error) {
    failures.push(`${name}: ${error.message}`);
  }
}

const full = JSON.parse(
  readFileSync(join(helpDir, "canonical/help-corpus.v1.json"), "utf8"),
);
const publicBytes = readFileSync(join(helpDir, "canonical/help-corpus-public.v1.json"), "utf8");
const publicCorpus = JSON.parse(publicBytes);

check("the public corpus carries only public records", () => {
  for (const kind of ["sources", "articles", "chunks"]) {
    for (const record of publicCorpus[kind]) {
      if (record.visibility !== "public") {
        throw new Error(`${kind} ${record.id} is ${record.visibility}`);
      }
    }
  }
});

check("the public corpus omits restricted text, not merely its index", () => {
  const restrictedChunks = full.chunks.filter((chunk) => chunk.visibility !== "public");
  if (restrictedChunks.length === 0) {
    throw new Error("the full corpus has no restricted chunk, so this check proves nothing");
  }
  for (const chunk of restrictedChunks) {
    if (publicBytes.includes(chunk.text)) {
      throw new Error(`restricted chunk ${chunk.id} appears verbatim in the public corpus`);
    }
  }
  for (const source of full.sources.filter((entry) => entry.visibility !== "public")) {
    if (publicBytes.includes(source.id)) {
      throw new Error(`restricted source ${source.id} is named in the public corpus`);
    }
  }
});

check("the public surface reaches no transport or authority module", () => {
  // A bundler follows imports, so one edge from anything reachable here to the
  // host transport would pull `invoke` into a shipped bundle regardless of what
  // names the surface re-exports.
  const forbidden = [/@tauri-apps/, /(^|\/)host($|\.)/];
  const seen = new Set();
  const walk = (file) => {
    if (seen.has(file)) return;
    seen.add(file);
    const source = readFileSync(file, "utf8");
    for (const [, specifier] of source.matchAll(/from\s+"([^"]+)"/g)) {
      if (forbidden.some((pattern) => pattern.test(specifier))) {
        throw new Error(`${file} imports ${specifier}`);
      }
      if (!specifier.startsWith(".") || specifier.endsWith(".json")) continue;
      walk(resolve(dirname(file), `${specifier}.ts`));
    }
  };
  walk(join(helpDir, "publicSurface.ts"));
  if (seen.size < 4) throw new Error("the import walk did not traverse the surface");
});

check("the built renderer bundle carries no restricted Help content", () => {
  const assets = join(desktop, "dist/assets");
  if (!existsSync(assets)) {
    throw new Error("dist/assets is missing — run `npm run build` first");
  }
  const bundles = readdirSync(assets)
    .filter((name) => name.endsWith(".js"))
    .map((name) => readFileSync(join(assets, name), "utf8"));
  if (bundles.length === 0) throw new Error("no built bundle to inspect");

  const restricted = full.chunks.filter((chunk) => chunk.visibility !== "public");
  for (const chunk of restricted) {
    // Compare on a distinctive slice: minification rewrites nothing inside a
    // string literal, but line breaks in the source text can be re-escaped.
    const probe = chunk.text.slice(0, 48);
    if (probe.length < 24) continue;
    for (const bundle of bundles) {
      if (bundle.includes(probe)) {
        throw new Error(`restricted chunk ${chunk.id} is present in the built bundle`);
      }
    }
  }
});

check("every digest domain the corpus records is covered by a parity case", () => {
  // `helpDigest.test.ts` proves TypeScript reproduces each recorded digest.
  // What it cannot prove is coverage: a domain nobody wrote a case for passes
  // that suite by not being in it. This checks the vocabulary instead.
  const parity = JSON.parse(readFileSync(join(helpDir, "generated/digest-parity.json"), "utf8"));
  const cases = parity.domainDigests;
  if (!Array.isArray(cases) || cases.length === 0) {
    throw new Error("parity artifact names no domain cases");
  }
  const covered = new Set(cases.map((entry) => entry.domain));

  const digestSource = readFileSync(join(helpDir, "canonical/digest.ts"), "utf8");
  const declared = [...digestSource.matchAll(/"(grokptah\.help\.[a-z-]+\.v1)"/g)].map(
    (match) => match[1],
  );
  if (declared.length === 0) throw new Error("no digest domains found in digest.ts");

  // Domains that describe a *set* rather than one record are digested from
  // member digests, so a fields/lengthPrefixed case cannot represent them.
  const structural = new Set([
    "grokptah.help.source-set.v1",
    "grokptah.help.corpus.v1",
    "grokptah.help.admission.v1",
  ]);
  const missing = declared.filter((domain) => !covered.has(domain) && !structural.has(domain));
  if (missing.length > 0) {
    throw new Error(`digest domains with no parity case: ${missing.join(", ")}`);
  }
});

for (const name of checks) console.log(`ok    ${name}`);
for (const failure of failures) console.error(`FAIL  ${failure}`);
if (failures.length > 0) process.exit(1);
console.log(`Help public projection verified: ${checks.length} checks.`);
