/**
 * What the published package may and may not contain.
 *
 * These are asserted against the module's actual exports rather than reviewed
 * by eye, because the failure mode is silent: a barrel that re-exports one
 * module too many ships authority to every consumer at once, and nothing at
 * build time notices.
 */

import { describe, expect, it } from "vitest";

import * as publicSurface from "./publicSurface";
import * as uiCore from "../uiCore";
import { HELP_PUBLIC_CORPUS, HelpBundleNotPublicError, assertPublicOnly } from "./publicSurface";
// Read the private artifact directly. `src/lib` must never import it — that is
// the property under test — but a test file is not bundled and needs both
// corpora in hand to compare them.
import fullCorpusJson from "./canonical/help-corpus.v1.json";
import type { HelpCorpus } from "./generated/contract";

const FULL = fullCorpusJson as unknown as HelpCorpus;

/** Names that must never be reachable from a published bundle. */
const FORBIDDEN_EXPORTS = [
  // authority constructors
  "issueGrant",
  "authorizeHelpDecision",
  "authorizeHelpDecisionJson",
  "parseHelpDecisionRequest",
  "buildHelpManifest",
  "createHelpGrant",
  "createHelpAdmission",
  // execution
  "createHelpExecutor",
  "HelpExecutor",
  "runHelpTask",
  // transport and route selection
  "helpAsk",
  "helpFollow",
  "helpCancel",
  "helpBounds",
  "helpSession",
  "helpVisibleCorpus",
  "requestHelpAnswer",
  "HelpAnswerTransport",
  "selectHelpRoute",
  "invoke",
];

describe("the published Help surface", () => {
  it("exports no authority constructor, executor, or transport", () => {
    const exported = Object.keys(publicSurface);
    for (const forbidden of FORBIDDEN_EXPORTS) {
      expect(exported, `publicSurface exports ${forbidden}`).not.toContain(forbidden);
    }
  });

  it("keeps them out of the ui-core barrel too", () => {
    const exported = Object.keys(uiCore);
    for (const forbidden of FORBIDDEN_EXPORTS) {
      expect(exported, `uiCore exports ${forbidden}`).not.toContain(forbidden);
    }
  });

  it("exports the offline half a consumer actually needs", () => {
    const exported = Object.keys(publicSurface);
    for (const required of [
      "HELP_PUBLIC_CORPUS",
      "HELP_PUBLIC_CORPUS_DIGEST",
      "searchHelpCorpus",
      "verifyHelpProjection",
      "verifyHelpCorpus",
      "assertPublicOnly",
      "sha256Hex",
      "domainDigest",
    ]) {
      expect(exported, `publicSurface is missing ${required}`).toContain(required);
    }
  });

  it("ships a public-only corpus", () => {
    expect(() => assertPublicOnly(HELP_PUBLIC_CORPUS)).not.toThrow();
    expect(HELP_PUBLIC_CORPUS.articles.length).toBeGreaterThan(0);
  });

  it("does not ship the private corpus", () => {
    // The restricted text must be absent, not merely unlisted: a bundle that
    // carries it has leaked it whatever the index says.
    const serialized = JSON.stringify(HELP_PUBLIC_CORPUS);
    const restricted = FULL.chunks.filter((chunk) => chunk.visibility !== "public");
    expect(restricted.length).toBeGreaterThan(0);
    for (const chunk of restricted) {
      expect(serialized).not.toContain(chunk.text);
    }
    const restrictedSources = FULL.sources.filter((source) => source.visibility !== "public");
    for (const source of restrictedSources) {
      expect(serialized).not.toContain(source.id);
    }
  });

  it("rejects a bundle that carries a restricted record", () => {
    const smuggled: HelpCorpus = {
      ...HELP_PUBLIC_CORPUS,
      sources: [
        ...HELP_PUBLIC_CORPUS.sources,
        { ...HELP_PUBLIC_CORPUS.sources[0], id: "smuggled", visibility: "operator" },
      ],
    };
    expect(() => assertPublicOnly(smuggled)).toThrow(HelpBundleNotPublicError);
  });

  it("exposes no raw provider reply anywhere in its types or values", () => {
    for (const [name, value] of Object.entries(publicSurface)) {
      if (typeof value !== "string") continue;
      expect(name.toLowerCase()).not.toContain("reply");
      expect(name.toLowerCase()).not.toContain("prompt");
    }
  });
});
