import { describe, expect, it } from "vitest";
import * as publicSurface from "./public";
import {
  GrokPtahBrokerClient,
  GROKPTAH_BROKER_EXTERNAL_WORKER_ROUTES,
  HELP_CONTRACT,
  EXTERNAL_WORKER_CONTRACT,
  createExternalWorkerMonitor,
  parseBrokerErrorEnvelope,
  parseExternalWorkerListPage,
  parseExternalWorkerListQuery,
  parseExternalWorkerSummary,
  replaceExternalWorkerMonitor,
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
    expect(typeof parseExternalWorkerListQuery).toBe("function");
    expect(typeof parseExternalWorkerListPage).toBe("function");
    expect(typeof parseExternalWorkerSummary).toBe("function");
    expect(typeof parseBrokerErrorEnvelope).toBe("function");
    expect(GROKPTAH_BROKER_EXTERNAL_WORKER_ROUTES.some((route) => route.id === "list")).toBe(true);
    expect(GROKPTAH_BROKER_EXTERNAL_WORKER_ROUTES.some((route) => route.id === "archive")).toBe(true);
    expect(createExternalWorkerMonitor().lastSeq).toBe(-1);
    expect(typeof replaceExternalWorkerMonitor).toBe("function");
    expect("GrokPtahClient" in publicSurface).toBe(false);
  });
});
