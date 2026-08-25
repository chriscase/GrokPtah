import { describe, expect, it } from "vitest";
import {
  GrokPtahBrokerClient,
  HELP_CONTRACT,
  EXTERNAL_WORKER_CONTRACT,
  createExternalWorkerMonitor,
  parseCapabilitySet,
  promptQueueReducer,
} from "./public";

describe("public integration barrel", () => {
  it("exposes only transport-neutral consumer surfaces", () => {
    expect(typeof GrokPtahBrokerClient).toBe("function");
    expect(HELP_CONTRACT).toBe("grokptah.help.v1");
    expect(typeof parseCapabilitySet).toBe("function");
    expect(typeof promptQueueReducer).toBe("function");
    expect(EXTERNAL_WORKER_CONTRACT).toBe("grokptah.external-workers.v1");
    expect(createExternalWorkerMonitor().lastSeq).toBe(-1);
  });
});
