/**
 * Cross-implementation conformance for `grokptah.external-workers.v1`.
 *
 * The contract has three hand-written implementations: the Rust SDK validator,
 * the browser-safe TypeScript parser in `externalWorker.ts`, and the published
 * JSON Schema. Three implementations of one rule drift, and a case-by-case
 * parity assertion only ever proves agreement on the cases someone remembered
 * to write down twice.
 *
 * This file runs the published schema through a real JSON Schema validator —
 * not a regex comparison — over a corpus that the Rust tests read from the same
 * file, and asserts the TypeScript parser reaches the identical verdict on
 * every case. A rule that changes in one implementation and not the others
 * fails here rather than in a consumer.
 */
import Ajv2020 from "ajv/dist/2020";
import { describe, expect, it } from "vitest";
import schema from "../../../docs/schemas/grokptah-external-worker.v1.schema.json";
import corpus from "../../../docs/schemas/grokptah-external-worker.v1.conformance.json";
import {
  EXTERNAL_WORKER_CONTRACT,
  parseExternalWorkerArtifact,
  parseExternalWorkerArtifactListing,
  MAX_EXTERNAL_WORKER_ARTIFACTS,
} from "./externalWorker";

// `strict` is deliberately on: a schema that leans on an unknown keyword is
// asserting something no validator enforces.
const ajv = new Ajv2020({ strict: true, allErrors: true });
const validateArtifact = ajv.compile({
  ...schema.$defs.artifact,
  $defs: schema.$defs,
});
const validateEnvelope = ajv.compile(schema);

function artifact(overrides: Record<string, unknown> = {}) {
  return {
    path: "artifacts/report.md",
    digest: corpus.validDigest,
    runId: "run-1",
    ...overrides,
  };
}

describe("external worker v1 conformance", () => {
  it("declares the same contract version as the schema and the client", () => {
    expect(corpus.contract).toBe(schema.properties.contract.const);
    expect(corpus.contract).toBe(EXTERNAL_WORKER_CONTRACT);
  });

  it("compiles the published schema under a real validator", () => {
    // A schema that does not compile has been asserting nothing.
    expect(typeof validateArtifact).toBe("function");
    expect(typeof validateEnvelope).toBe("function");
  });

  it("agrees with the schema validator on every artifact path in the corpus", () => {
    for (const path of corpus.artifactPath.accept) {
      const candidate = artifact({ path });
      expect(validateArtifact(candidate), `schema must accept ${JSON.stringify(path)}`).toBe(true);
      expect(
        parseExternalWorkerArtifact(candidate),
        `parser must accept ${JSON.stringify(path)}`,
      ).not.toBeNull();
    }
    for (const path of corpus.artifactPath.refuse) {
      const candidate = artifact({ path });
      expect(validateArtifact(candidate), `schema must refuse ${JSON.stringify(path)}`).toBe(false);
      expect(
        parseExternalWorkerArtifact(candidate),
        `parser must refuse ${JSON.stringify(path)}`,
      ).toBeNull();
    }
  });

  it("agrees with the schema validator on every digest in the corpus", () => {
    for (const digest of corpus.digest.accept) {
      const candidate = artifact({ digest });
      expect(validateArtifact(candidate)).toBe(true);
      expect(parseExternalWorkerArtifact(candidate)).not.toBeNull();
    }
    for (const digest of corpus.digest.refuse) {
      const candidate = artifact({ digest });
      expect(validateArtifact(candidate), `schema must refuse ${JSON.stringify(digest)}`).toBe(false);
      expect(
        parseExternalWorkerArtifact(candidate),
        `parser must refuse ${JSON.stringify(digest)}`,
      ).toBeNull();
    }
  });

  it("agrees with the schema validator on every reported size in the corpus", () => {
    for (const sizeBytes of corpus.sizeBytes.accept) {
      const candidate = artifact({ sizeBytes });
      expect(validateArtifact(candidate), `schema must accept ${sizeBytes}`).toBe(true);
      expect(parseExternalWorkerArtifact(candidate), `parser must accept ${sizeBytes}`).not.toBeNull();
    }
    for (const sizeBytes of corpus.sizeBytes.refuse) {
      const candidate = artifact({ sizeBytes });
      expect(validateArtifact(candidate), `schema must refuse ${sizeBytes}`).toBe(false);
      expect(parseExternalWorkerArtifact(candidate), `parser must refuse ${sizeBytes}`).toBeNull();
    }
  });

  it("bounds a listing identically in the schema and the parser", () => {
    const envelope = (count: number) => ({
      contract: corpus.contract,
      launchRequest: {
        requestId: "req-1",
        provider: "cursor_cloud",
        repository: "org/repo",
        startingRef: "main",
        prompt: "do the work",
        executionMode: "isolated",
        autoCreatePr: false,
      },
      worker: {
        provider: "cursor_cloud",
        externalAgentId: "agent-1",
        repository: "org/repo",
        startingRef: "main",
        state: "running",
        createdAt: "2026-08-25T00:00:00Z",
        updatedAt: "2026-08-25T00:00:00Z",
      },
      run: {
        externalAgentId: "agent-1",
        externalRunId: "run-1",
        state: "running",
        stream: "unsupported",
        lastSeq: null,
        createdAt: "2026-08-25T00:00:00Z",
        updatedAt: "2026-08-25T00:00:00Z",
      },
      artifacts: Array.from({ length: count }, () => artifact()),
    });

    expect(validateEnvelope(envelope(MAX_EXTERNAL_WORKER_ARTIFACTS))).toBe(true);
    expect(validateEnvelope(envelope(MAX_EXTERNAL_WORKER_ARTIFACTS + 1))).toBe(false);
    expect(
      parseExternalWorkerArtifactListing(envelope(MAX_EXTERNAL_WORKER_ARTIFACTS).artifacts, "run-1"),
    ).toHaveLength(MAX_EXTERNAL_WORKER_ARTIFACTS);
    expect(
      parseExternalWorkerArtifactListing(
        envelope(MAX_EXTERNAL_WORKER_ARTIFACTS + 1).artifacts,
        "run-1",
      ),
    ).toBeNull();
  });
});
