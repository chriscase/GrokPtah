/**
 * The private corpus must not be reachable from app code.
 *
 * This is the gate that was missing. An earlier version of `publicSurface`
 * imported its verifier from a module that loaded `help-corpus.v1.json` at the
 * top level. Nothing exported the full corpus, every export-level assertion
 * passed, and all 27 restricted chunks were still emitted into the published
 * bundles — because a bundler follows imports, not export lists.
 *
 * `scripts/verify-public.mjs` catches it in the built artifact, which is the
 * authoritative check but needs a build. This one is static, runs in
 * milliseconds, and names the offending file.
 */

import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const HERE = dirname(fileURLToPath(import.meta.url));
const LIB_ROOT = resolve(HERE, "..");

/** The artifact only Rust and tests may read. */
const PRIVATE_ARTIFACT = "help-corpus.v1.json";

/**
 * Whether `source` actually *imports* the private artifact.
 *
 * Matching the bare filename would flag the doc comments that explain why a
 * module does not import it, so only import specifiers count — which is also
 * what a bundler follows.
 */
function importsPrivateCorpus(source: string): boolean {
  const specifier = /(?:from|import)\s*\(?\s*["'][^"']*help-corpus\.v1\.json["']/;
  return specifier.test(source);
}

function sourceFiles(directory: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(directory)) {
    const full = join(directory, entry);
    if (statSync(full).isDirectory()) {
      out.push(...sourceFiles(full));
      continue;
    }
    if (!/\.(ts|tsx)$/.test(entry)) continue;
    // Test files are never bundled.
    if (/\.test\.tsx?$/.test(entry)) continue;
    out.push(full);
  }
  return out;
}

describe("published bundle hygiene", () => {
  it("no shipped module imports the private corpus", () => {
    const offenders = sourceFiles(LIB_ROOT)
      .filter((file) => importsPrivateCorpus(readFileSync(file, "utf8")))
      .map((file) => relative(LIB_ROOT, file));
    expect(
      offenders,
      `these modules import ${PRIVATE_ARTIFACT}. A bundler follows imports, so every ` +
        `bundle downstream of them carries the restricted text whether or not it is ` +
        `exported. Rust embeds that artifact; TypeScript reads the public one.`,
    ).toEqual([]);
  });

  it("no component imports the private corpus either", () => {
    const components = resolve(LIB_ROOT, "..", "components");
    const offenders = sourceFiles(components)
      .filter((file) => importsPrivateCorpus(readFileSync(file, "utf8")))
      .map((file) => relative(components, file));
    expect(offenders).toEqual([]);
  });

  it("the corpus module that app code uses reads the public artifact", () => {
    const corpusModule = readFileSync(join(LIB_ROOT, "help/canonical/corpus.ts"), "utf8");
    expect(corpusModule).toContain("help-corpus-public.v1.json");
    expect(importsPrivateCorpus(corpusModule)).toBe(false);
  });

  it("the verifier carries no corpus of its own", () => {
    // A verifier that imports a corpus drags it into everything that verifies.
    const verifier = readFileSync(join(LIB_ROOT, "help/canonical/verify.ts"), "utf8");
    expect(importsPrivateCorpus(verifier)).toBe(false);
    expect(verifier).not.toContain("help-corpus-public.v1.json");
  });

  it("the published surface does not import the Tauri transport", () => {
    const surface = readFileSync(join(LIB_ROOT, "help/publicSurface.ts"), "utf8");
    expect(surface).not.toContain("./host");
    expect(surface).not.toContain("@tauri-apps");
  });
});
