import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  HELP_MAX_ID_BYTES,
  HELP_MAX_SOURCES_PER_DECISION,
  HelpAuthorityMalformedError,
  authorizeHelpDecision,
  authorizeHelpDecisionJson,
  parseHelpDecisionRequest,
  type HelpDecisionRequest,
} from "./authority/decision";

/**
 * The same fixture set the Rust crate executes. If the two implementations
 * ever disagree on a case, one of these suites fails.
 */
/**
 * Resolved from this file, not from the working directory.
 *
 * A cwd-relative path made the parity gate depend on where the runner was
 * invoked from: run from the repository root instead of `desktop/`, the
 * fixture simply was not there and the suite failed for a reason that had
 * nothing to do with the two implementations agreeing.
 */
const AUTHORITY_CRATE = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "..", "..", "..", "..",
  "crates", "common", "grokptah-help-authority",
);

const FIXTURES = JSON.parse(
  readFileSync(resolve(AUTHORITY_CRATE, "fixtures", "authority-parity.json"), "utf8"),
) as {
  servedCorpusDigest: string;
  servedIndexDigest: string;
  cases: Array<{
    name: string;
    request: unknown;
    expect: {
      parses: boolean;
      allowed?: boolean;
      deniedBecause?: string | null;
      allowedSourceIds?: string[];
      deniedSourceIds?: string[];
    };
  }>;
};

const SCHEMA = JSON.parse(
  readFileSync(resolve(AUTHORITY_CRATE, "schema", "help-authority.v1.schema.json"), "utf8"),
) as Record<string, any>;

describe("Help authority parity with the Rust reference", () => {
  it("executes a meaningful number of shared cases", () => {
    expect(FIXTURES.cases.length).toBeGreaterThanOrEqual(20);
    expect(new Set(FIXTURES.cases.map((entry) => entry.name)).size).toBe(FIXTURES.cases.length);
  });

  it.each(FIXTURES.cases.map((entry) => [entry.name, entry] as const))(
    "agrees with Rust: %s",
    (_name, entry) => {
      const payload = JSON.stringify(entry.request);
      if (!entry.expect.parses) {
        expect(() =>
          authorizeHelpDecisionJson(payload, FIXTURES.servedCorpusDigest, FIXTURES.servedIndexDigest),
        ).toThrow(HelpAuthorityMalformedError);
        return;
      }
      const response = authorizeHelpDecisionJson(
        payload,
        FIXTURES.servedCorpusDigest,
        FIXTURES.servedIndexDigest,
      );
      expect(response.allowed).toBe(entry.expect.allowed);
      expect(response.denied_because ?? null).toBe(entry.expect.deniedBecause ?? null);
      expect([...response.receipt.allowed_source_ids]).toEqual(entry.expect.allowedSourceIds ?? []);
      expect(response.receipt.denied.map((decision) => decision.source_id)).toEqual(
        entry.expect.deniedSourceIds ?? [],
      );
    },
  );
});

describe("Help authority closed contract", () => {
  it("rejects unknown fields rather than dropping them", () => {
    const base = {
      schema: "grokptah.help-authority-request.v1",
      action: "search",
      principal: { principal_id: "a", tenant_id: "t", capabilities: ["help_search"] },
      corpus_digest: "d",
      index_digest: "i",
    };
    for (const mutation of [
      { ...base, bypassAuthority: true },
      { ...base, principal: { ...base.principal, isAdmin: true } },
      { ...base, sources: [{ source_id: "s", visibility: "public", tenant_id: "t", digest: "d", extra: 1 }] },
      { ...base, sources: [{ source_id: "s", visibility: "internal", tenant_id: "t", digest: "d" }] },
      { ...base, principal: { ...base.principal, capabilities: ["help_admin"] } },
      { ...base, action: "escalate" },
    ]) {
      expect(() => parseHelpDecisionRequest(mutation), JSON.stringify(mutation)).toThrow(
        HelpAuthorityMalformedError,
      );
    }
  });

  it("counts identifier bounds in UTF-8 bytes, as Rust does", () => {
    // A string of multi-byte characters is shorter in code units than bytes;
    // counting code units would let an oversized id through on this side only.
    const multiByte = "é".repeat(HELP_MAX_ID_BYTES);
    expect(multiByte.length).toBeLessThan(HELP_MAX_ID_BYTES * 2);
    const request = parseHelpDecisionRequest({
      schema: "grokptah.help-authority-request.v1",
      action: "search",
      principal: { principal_id: multiByte, tenant_id: "t", capabilities: ["help_search"] },
      corpus_digest: "c",
      index_digest: "i",
    });
    expect(authorizeHelpDecision(request, "c", "i").denied_because).toBe("bounds");
  });
});

describe("Help authority receipts", () => {
  function decide(sources: unknown[], capabilities: string[]): ReturnType<typeof authorizeHelpDecision> {
    const request = parseHelpDecisionRequest({
      schema: "grokptah.help-authority-request.v1",
      action: "search",
      principal: { principal_id: "alice", tenant_id: "tenant-a", project_ids: ["proj-1"], capabilities },
      corpus_digest: "c",
      index_digest: "i",
      sources,
    });
    return authorizeHelpDecision(request, "c", "i");
  }

  it("carries no path, heading, content, or query text", () => {
    const response = decide(
      [
        { source_id: "pub-1", visibility: "public", tenant_id: "tenant-a", digest: "d" },
        { source_id: "priv-1", visibility: "private", tenant_id: "tenant-a", owner_principal_id: "mallory", digest: "d" },
      ],
      ["help_search", "help_search_private"],
    );
    const serialized = JSON.stringify(response.receipt);
    for (const forbidden of ["docs/", "README", ".md", "/Users/", "/home/", "heading", "path", "content", "query"]) {
      expect(serialized, forbidden).not.toContain(forbidden);
    }
  });

  it("produces a digest that is deterministic and input-sensitive", () => {
    const first = decide([{ source_id: "pub-1", visibility: "public", tenant_id: "tenant-a", digest: "d" }], ["help_search"]);
    const again = decide([{ source_id: "pub-1", visibility: "public", tenant_id: "tenant-a", digest: "d" }], ["help_search"]);
    const other = decide([{ source_id: "pub-2", visibility: "public", tenant_id: "tenant-a", digest: "d" }], ["help_search"]);
    expect(again.receipt.receipt_digest).toBe(first.receipt.receipt_digest);
    expect(other.receipt.receipt_digest).not.toBe(first.receipt.receipt_digest);
  });

  it("does not grow the receipt with an oversized request", () => {
    const sources = Array.from({ length: HELP_MAX_SOURCES_PER_DECISION + 40 }, (_unused, index) => ({
      source_id: `pub-${index}`,
      visibility: "public",
      tenant_id: "tenant-a",
      digest: "d",
    }));
    const response = decide(sources, ["help_search"]);
    expect(response.allowed).toBe(false);
    expect(response.denied_because).toBe("bounds");
    expect(response.receipt.denied.length).toBeLessThanOrEqual(HELP_MAX_SOURCES_PER_DECISION);
  });
});

describe("Help authority schema document", () => {
  it("closes every object it defines", () => {
    for (const [name, definition] of Object.entries(SCHEMA.$defs as Record<string, any>)) {
      if (definition?.type === "object") {
        expect(definition.additionalProperties, name).toBe(false);
      }
    }
  });

  it("declares the same enum members this implementation accepts", () => {
    expect(SCHEMA.$defs.visibility.enum).toEqual(["public", "project", "private"]);
    expect(SCHEMA.$defs.action.enum).toEqual(["search", "answer", "read_source"]);
    expect(SCHEMA.$defs.capability.enum).toEqual([
      "help_search",
      "help_search_project",
      "help_search_private",
      "help_answer",
    ]);
    expect(SCHEMA.$defs.denyReason.enum).toEqual([
      "unknown_schema",
      "missing_capability",
      "tenant_mismatch",
      "scope_mismatch",
      "malformed_scope",
      "stale_index",
      "bounds",
    ]);
  });

  it("declares the same bounds as both implementations", () => {
    expect(SCHEMA.$defs.boundedId.maxLength).toBe(HELP_MAX_ID_BYTES);
    expect(SCHEMA.$defs.request.properties.sources.maxItems).toBe(HELP_MAX_SOURCES_PER_DECISION);
  });
});
