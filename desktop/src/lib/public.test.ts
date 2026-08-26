import { describe, expect, it } from "vitest";
import {
  GrokPtahBrokerClient,
  HELP_PUBLIC_CORPUS_DIGEST,
  EXTERNAL_WORKER_CONTRACT,
  createExternalWorkerMonitor,
  parseCapabilitySet,
  promptQueueReducer,
} from "./public";

describe("public integration barrel", () => {
  it("exposes only transport-neutral consumer surfaces", () => {
    expect(typeof GrokPtahBrokerClient).toBe("function");
    // Help now ships a digest-bound corpus rather than a bare contract
    // string. The digest is what a consumer can actually check.
    expect(HELP_PUBLIC_CORPUS_DIGEST).toMatch(/^sha256:[0-9a-f]{64}$/);
    expect(typeof parseCapabilitySet).toBe("function");
    expect(typeof promptQueueReducer).toBe("function");
    expect(EXTERNAL_WORKER_CONTRACT).toBe("grokptah.external-workers.v1");
    expect(createExternalWorkerMonitor().lastSeq).toBe(-1);
  });
});
