import { describe, expect, it } from "vitest";
import { effortForModel, effortOptionsForModel } from "./modelOptions";
import type { ModelInfo } from "./protocol";

const models: ModelInfo[] = [
  {
    id: "grok-4.5",
    display_name: "Grok 4.5",
    supports_effort: true,
    effort_options: ["low", "medium", "high"],
  },
  {
    id: "plain",
    display_name: "Plain",
    supports_effort: false,
  },
  {
    id: "legacy-reasoning",
    display_name: "Legacy reasoning",
    supports_effort: true,
  },
];

describe("model effort options", () => {
  it("uses the server-advertised values", () => {
    expect(effortOptionsForModel(models, "grok-4.5")).toEqual([
      "low",
      "medium",
      "high",
    ]);
    expect(effortForModel(models, "grok-4.5", "xhigh")).toBe("low");
  });

  it("offers only none for models without reasoning effort", () => {
    expect(effortOptionsForModel(models, "plain")).toEqual(["none"]);
    expect(effortForModel(models, "plain", "high")).toBe("none");
  });

  it("keeps the legacy fallback for older catalogs", () => {
    expect(effortOptionsForModel(models, "legacy-reasoning")).toEqual([
      "low",
      "medium",
      "high",
      "xhigh",
    ]);
  });
});
