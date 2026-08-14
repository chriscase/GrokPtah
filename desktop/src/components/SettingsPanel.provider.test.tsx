import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

import { SettingsPanel } from "./SettingsPanel";
import { api } from "../lib/api";

vi.mock("../lib/api", () => ({
  api: {
    settingsSnapshot: vi.fn(),
    upsertProviderProfile: vi.fn(),
    discoverProviderModels: vi.fn(),
    qualifyProviderModel: vi.fn(),
    deleteProviderProfile: vi.fn(),
  },
}));

const profile = {
  id: "company-gateway",
  label: "Company gateway",
  baseUrl: "https://gateway.example/v1",
  deadlineClass: "standard" as const,
  credentialSet: true,
  managedByEnv: false,
  models: [
    {
      id: "Team/Code:Cheap",
      displayName: "Team/Code:Cheap",
      supportsTools: false,
      supportsStream: false,
      supportsImageInput: false,
      computerUseTier: "none" as const,
      computerCapabilitySource: "unknown" as const,
      effortOptions: ["low"],
      capabilitySource: "unknown" as const,
    },
  ],
};

describe("provider profile settings (#278)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.settingsSnapshot).mockResolvedValue({
      model: "grok-4.5",
      effort: "none",
      gatewayProviderId: profile.id,
      gatewayProfiles: [profile],
    });
    vi.mocked(api.upsertProviderProfile).mockResolvedValue(undefined);
    vi.mocked(api.discoverProviderModels).mockResolvedValue([]);
    vi.mocked(api.qualifyProviderModel).mockResolvedValue({
      providerId: profile.id,
      modelId: profile.models[0].id,
      basicGeneration: { status: "pass", detail: "Basic generation passed" },
      nativeToolCall: { status: "fail", detail: "No native tool call" },
      toolResultContinuation: { status: "fail", detail: "Not attempted" },
      streaming: { status: "degraded", detail: "JSON response" },
      codingReady: false,
      semanticObservation: { status: "fail", detail: "Not attempted" },
      staleObservationRecovery: { status: "fail", detail: "Not attempted" },
      computerUseTier: "none",
    });
  });

  afterEach(cleanup);

  it("shows no secret and preserves an opaque manual model id through qualification", async () => {
    render(
      <SettingsPanel
        open
        onClose={() => {}}
        models={[]}
        auth={{ signed_in: false }}
        onAuthChange={() => {}}
        onChromeChange={() => {}}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Auth" }));
    await screen.findByTestId("settings-gateway");
    const key = screen.getByTestId("gateway-api-key") as HTMLInputElement;
    expect(key.type).toBe("password");
    expect(key.value).toBe("");
    expect(key.placeholder).toContain("Saved");

    const model = screen.getByTestId("gateway-model-id") as HTMLInputElement;
    fireEvent.change(model, { target: { value: " Team/Code:Cheap " } });
    fireEvent.click(screen.getByTestId("gateway-qualify"));

    await waitFor(() =>
      expect(api.upsertProviderProfile).toHaveBeenCalledWith(
        profile.id,
        profile.label,
        profile.baseUrl,
        " Team/Code:Cheap ",
        "standard",
        [],
        null,
      ),
    );
    expect(api.qualifyProviderModel).toHaveBeenCalledWith(
      profile.id,
      " Team/Code:Cheap ",
    );
    expect(await screen.findByText(/Discussion only/)).toHaveTextContent(
      /native tools: No native tool call/,
    );
  });

  it("reports semantic Computer eligibility only after stale-frame recovery", async () => {
    vi.mocked(api.qualifyProviderModel).mockResolvedValue({
      providerId: profile.id,
      modelId: profile.models[0].id,
      basicGeneration: { status: "pass", detail: "Basic generation passed" },
      nativeToolCall: { status: "pass", detail: "Native tool call passed" },
      toolResultContinuation: { status: "pass", detail: "Continuation passed" },
      streaming: { status: "pass", detail: "Streaming passed" },
      codingReady: true,
      semanticObservation: { status: "pass", detail: "Exact element selected" },
      staleObservationRecovery: { status: "pass", detail: "Replacement frame used" },
      computerUseTier: "semantic_act",
    });
    render(
      <SettingsPanel
        open
        onClose={() => {}}
        models={[]}
        auth={{ signed_in: false }}
        onAuthChange={() => {}}
        onChromeChange={() => {}}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Auth" }));
    await screen.findByTestId("settings-gateway");
    fireEvent.click(screen.getByTestId("gateway-qualify"));

    expect(
      await screen.findByText("Qualified for coding tools and semantic Computer Use"),
    ).toBeTruthy();
  });
});
