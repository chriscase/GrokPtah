import { describe, expect, it } from "vitest";
import {
  GrokPtahBrokerClient,
  HELP_CORPUS_VERSION,
  EXTERNAL_WORKER_CONTRACT,
  createExternalWorkerMonitor,
  parseCapabilitySet,
  promptQueueReducer,
} from "./public";

describe("public integration barrel", () => {
  it("exposes only transport-neutral consumer surfaces", () => {
    expect(typeof GrokPtahBrokerClient).toBe("function");
    // The live, source-cited corpus — not the access-gated grokptah.help.v1
    // corpus, which browser consumers must not be able to bind.
    expect(HELP_CORPUS_VERSION).toBe("product-corpus-v1");
    expect(typeof parseCapabilitySet).toBe("function");
    expect(typeof promptQueueReducer).toBe("function");
    expect(EXTERNAL_WORKER_CONTRACT).toBe("grokptah.external-workers.v1");
    expect(createExternalWorkerMonitor().lastSeq).toBe(-1);
  });
});
