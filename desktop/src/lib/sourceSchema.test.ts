import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
  SOURCE_VIEW_ERROR_CODES,
  parseSourceDocument,
  parseSourceReadCursor,
  parseSourceRootDescriptor,
  parseSourceRootSnapshot,
  sourceViewErrorSummary,
} from "./sourceView";

/**
 * The Rust crate, this parser, and the JSON Schema all describe one contract.
 * These tests hold them together: the golden fixtures are the shared artefact,
 * and each side is checked against them rather than against the others'
 * behaviour.
 */

const ROOT = resolve(process.cwd(), "..");

function readJson(relative: string): Record<string, unknown> {
  return JSON.parse(readFileSync(resolve(ROOT, relative), "utf8"));
}

const fixtures = readJson("docs/schemas/grokptah-source-view.v1.fixtures.json") as {
  contract: string;
  errors: Array<Record<string, unknown>>;
  valid: Record<string, unknown[]>;
  invalid: Record<string, Array<{ reason: string; value: unknown }>>;
};
const schema = readJson("docs/schemas/grokptah-source-view.v1.schema.json") as {
  $defs: Record<string, { properties?: Record<string, unknown>; required?: string[] }>;
};
const rustError = readFileSync(
  resolve(ROOT, "crates/common/xai-source-view/src/error.rs"),
  "utf8",
);

const PARSERS: Record<string, (value: unknown) => unknown> = {
  rootDescriptor: parseSourceRootDescriptor,
  rootSnapshot: parseSourceRootSnapshot,
  readCursor: parseSourceReadCursor,
  sourceDocument: parseSourceDocument,
};

describe("shared golden fixtures", () => {
  it("accepts every valid fixture", () => {
    for (const [kind, entries] of Object.entries(fixtures.valid)) {
      const parse = PARSERS[kind];
      expect(parse, `no parser for fixture group ${kind}`).toBeTypeOf("function");
      for (const entry of entries) {
        expect(() => parse(entry), `${kind} fixture must parse: ${JSON.stringify(entry)}`).not.toThrow();
      }
    }
  });

  it("refuses every invalid fixture, for the stated reason", () => {
    for (const [kind, entries] of Object.entries(fixtures.invalid)) {
      const parse = PARSERS[kind];
      if (!parse) continue;
      for (const entry of entries) {
        expect(
          () => parse(entry.value),
          `${kind} fixture should have been refused (${entry.reason})`,
        ).toThrow();
      }
    }
  });

  it("covers every group the schema defines a parser for", () => {
    for (const kind of Object.keys(PARSERS)) {
      expect(fixtures.valid[kind]?.length ?? 0).toBeGreaterThan(0);
      expect(fixtures.invalid[kind]?.length ?? 0).toBeGreaterThan(0);
    }
  });
});

describe("closed error contract", () => {
  it("matches the Rust code list exactly", () => {
    const declared = rustError
      .slice(rustError.indexOf("pub const CODES"))
      .split("];")[0]
      .match(/"([a-z_]+)"/g)
      ?.map((quoted) => quoted.slice(1, -1));
    expect(declared, "could not read CODES from the Rust contract").toBeTruthy();
    expect([...(declared as string[])].sort()).toEqual([...SOURCE_VIEW_ERROR_CODES].sort());
  });

  it("matches the JSON Schema enumeration exactly", () => {
    const enumeration = (schema.$defs.errorCode as { enum: string[] }).enum;
    expect([...enumeration].sort()).toEqual([...SOURCE_VIEW_ERROR_CODES].sort());
  });

  it("matches the golden error fixtures exactly", () => {
    const covered = fixtures.errors.map((entry) => entry.code as string);
    expect([...covered].sort()).toEqual([...SOURCE_VIEW_ERROR_CODES].sort());
  });

  it("explains every golden error without falling through to the generic text", () => {
    for (const entry of fixtures.errors) {
      const summary = sourceViewErrorSummary(`${entry.code}: detail`);
      expect(summary).not.toBe("The file could not be opened.");
    }
  });

  it("carries no absolute path or content in any golden error", () => {
    for (const entry of fixtures.errors) {
      const rendered = JSON.stringify(entry);
      expect(rendered).not.toMatch(/"\/[A-Za-z]/);
      expect(rendered).not.toMatch(/[A-Za-z]:\\\\/);
      for (const forbidden of ["path", "absolutePath", "rootPath", "cwd"]) {
        expect(Object.keys(entry)).not.toContain(forbidden);
      }
    }
  });
});

describe("closed payload shapes", () => {
  it("keeps the schema and the parser in agreement about required keys", () => {
    const cases: Array<[string, readonly string[]]> = [
      ["rootDescriptor", ["token", "kind", "label", "pathDigest", "identityDigest", "runId"]],
      [
        "rootSnapshot",
        [
          "snapshotId",
          "revision",
          "issuedAtMs",
          "expiresAtMs",
          "principalFingerprint",
          "policyFingerprint",
          "replayPolicy",
          "roots",
        ],
      ],
      ["readCursor", ["byteOffset", "nextLineNumber", "carryHex", "continuesLine", "documentDigest"]],
      [
        "sourceDocument",
        [
          "contract",
          "root",
          "snapshotId",
          "revision",
          "relativePath",
          "language",
          "byteLen",
          "content",
          "identity",
          "limits",
          "chunk",
        ],
      ],
    ];
    for (const [kind, expected] of cases) {
      expect([...(schema.$defs[kind].required ?? [])].sort()).toEqual([...expected].sort());
    }
  });

  it("declares every payload closed to unknown fields", () => {
    for (const name of [
      "errorEnvelope",
      "rootDescriptor",
      "rootSnapshot",
      "readCursor",
      "contentClass",
      "effectiveLimits",
      "sourceLine",
      "sourceChunk",
      "sourceDocument",
    ]) {
      expect(
        (schema.$defs[name] as { additionalProperties?: boolean }).additionalProperties,
        `${name} must be closed`,
      ).toBe(false);
    }
  });

  it("bounds every integer the schema publishes", () => {
    const walk = (node: unknown): void => {
      if (!node || typeof node !== "object") return;
      const record = node as Record<string, unknown>;
      if (record.type === "integer") {
        expect(record.minimum, `integer without a floor: ${JSON.stringify(record)}`).toBeTypeOf(
          "number",
        );
        expect(record.maximum, `integer without a ceiling: ${JSON.stringify(record)}`).toBeTypeOf(
          "number",
        );
        expect(record.maximum as number).toBeLessThanOrEqual(Number.MAX_SAFE_INTEGER);
      }
      for (const value of Object.values(record)) walk(value);
    };
    walk(schema);
  });
});
