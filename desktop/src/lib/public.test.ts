import { describe, expect, it } from "vitest";
import {
  GrokPtahBrokerClient,
  HELP_CONTRACT,
  parseCapabilitySet,
  promptQueueReducer,
} from "./public";

describe("public integration barrel", () => {
  it("exposes only transport-neutral consumer surfaces", () => {
    expect(typeof GrokPtahBrokerClient).toBe("function");
    expect(HELP_CONTRACT).toBe("grokptah.help.v1");
    expect(typeof parseCapabilitySet).toBe("function");
    expect(typeof promptQueueReducer).toBe("function");
  });
});
