import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

import { SettingsPanel } from "./SettingsPanel";
import { api } from "../lib/api";
import { READINESS_VERDICT_LABEL } from "./ProviderReadinessCenter";

vi.mock("../lib/api", () => ({
  api: {
    settingsSnapshot: vi.fn(),
    upsertProviderProfile: vi.fn(),
    discoverProviderModels: vi.fn(),
    qualifyProviderModel: vi.fn(),
    deleteProviderProfile: vi.fn(),
  },
}));

const unknownModel = {
  id: "Team/Code:Cheap",
  displayName: "Team/Code:Cheap",
  supportsTools: false,
  supportsStream: false,
  supportsImageInput: false,
  computerUseTier: "none" as const,
  computerCapabilitySource: "unknown" as const,
  effortOptions: ["low"],
  capabilitySource: "unknown" as const,
};

const readyModel = {
  id: "Team/Code:Ready",
  displayName: "Team/Code:Ready",
  supportsTools: true,
  supportsStream: true,
  supportsImageInput: false,
  computerUseTier: "observe" as const,
  computerCapabilitySource: "measured" as const,
  effortOptions: ["low"],
  capabilitySource: "measured" as const,
};

const profile = {
  id: "company-gateway",
  label: "Company gateway",
  baseUrl: "https://gateway.example/v1",
  deadlineClass: "standard" as const,
  credentialSet: true,
  managedByEnv: false,
  models: [unknownModel],
};

const discussionReport = {
  providerId: profile.id,
  modelId: unknownModel.id,
  basicGeneration: { status: "pass" as const, detail: "Basic generation passed" },
  nativeToolCall: { status: "fail" as const, detail: "No native tool call" },
  toolResultContinuation: { status: "fail" as const, detail: "Not attempted" },
  streaming: { status: "degraded" as const, detail: "JSON response" },
  codingReady: false,
  semanticObservation: { status: "fail" as const, detail: "Not attempted" },
  staleObservationRecovery: { status: "fail" as const, detail: "Not attempted" },
  computerUseTier: "none" as const,
};

function renderSettings() {
  return render(
    <SettingsPanel
      open
      onClose={() => {}}
      models={[]}
      auth={{ signed_in: false }}
      onAuthChange={() => {}}
      onChromeChange={() => {}}
    />,
  );
}

async function openGateway() {
  renderSettings();
  fireEvent.click(screen.getByRole("button", { name: "Auth" }));
  return screen.findByTestId("settings-gateway");
}

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
    vi.mocked(api.qualifyProviderModel).mockResolvedValue(discussionReport);
  });

  afterEach(cleanup);

  it("shows no secret and preserves an opaque manual model id through qualification", async () => {
    await openGateway();
    const key = screen.getByTestId("gateway-api-key") as HTMLInputElement;
    expect(key.type).toBe("password");
    expect(key.value).toBe("");
    expect(key.placeholder).toContain("Saved");

    const model = screen.getByTestId("gateway-model-id") as HTMLInputElement;
    fireEvent.change(model, { target: { value: " Team/Code:Cheap " } });
    expect(screen.getByTestId("readiness-model-id")).toHaveTextContent(
      " Team/Code:Cheap ",
      { normalizeWhitespace: false },
    );
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
    expect(await screen.findByTestId("readiness-verdict")).toHaveTextContent(
      READINESS_VERDICT_LABEL.discussion_only,
    );
    expect(screen.getByTestId("readiness-blocking-reasons")).toHaveTextContent(
      /No native tool call/,
    );
    expect(screen.getByTestId("native-coding-readiness").textContent).not.toContain(
      "Saved (leave blank to keep)",
    );
    expect(screen.getByTestId("native-coding-readiness").textContent).not.toContain(
      "Provider key",
    );
  });

  it("reports semantic Computer eligibility independently of coding readiness", async () => {
    vi.mocked(api.qualifyProviderModel).mockResolvedValue({
      ...discussionReport,
      nativeToolCall: { status: "pass", detail: "Native tool call passed" },
      toolResultContinuation: { status: "pass", detail: "Continuation passed" },
      streaming: { status: "pass", detail: "Streaming passed" },
      codingReady: true,
      semanticObservation: { status: "pass", detail: "Exact element selected" },
      staleObservationRecovery: { status: "pass", detail: "Replacement frame used" },
      computerUseTier: "semantic_act",
    });
    await openGateway();
    fireEvent.click(screen.getByTestId("gateway-qualify"));

    expect(await screen.findByTestId("readiness-verdict")).toHaveTextContent(
      READINESS_VERDICT_LABEL.ready,
    );
    expect(screen.getByTestId("readiness-computer-tier")).toHaveTextContent(
      "Semantic act",
    );
    expect(screen.getByTestId("readiness-computer-tier")).toHaveTextContent(
      /independent/i,
    );
    expect(
      screen.queryByText("Qualified for coding tools and semantic Computer Use"),
    ).toBeNull();
    fireEvent.click(screen.getByText("Power-user evidence"));
    expect(screen.getByTestId("readiness-details")).toHaveTextContent(
      "Replacement frame used",
    );
  });

  it("clears stale qualification evidence when the selected model changes", async () => {
    vi.mocked(api.settingsSnapshot).mockResolvedValue({
      model: "grok-4.5",
      effort: "none",
      gatewayProviderId: profile.id,
      gatewayProfiles: [{ ...profile, models: [unknownModel, readyModel] }],
    });
    await openGateway();
    fireEvent.click(screen.getByTestId("gateway-qualify"));
    expect(await screen.findByTestId("readiness-verdict")).toHaveTextContent(
      READINESS_VERDICT_LABEL.discussion_only,
    );
    expect(screen.getByTestId("readiness-blocking-reasons")).toHaveTextContent(
      /No native tool call/,
    );

    fireEvent.click(screen.getByRole("button", { name: /Team\/Code:Ready/ }));
    expect(screen.getByTestId("readiness-verdict")).toHaveTextContent(
      READINESS_VERDICT_LABEL.ready,
    );
    expect(screen.queryByTestId("readiness-blocking-reasons")).toBeNull();
    expect(screen.getByTestId("readiness-computer-tier")).toHaveTextContent(
      "Observation only",
    );
    expect(screen.getByTestId("gateway-model-id")).toHaveValue(readyModel.id);
  });

  it("does not start a second qualification while one is in flight", async () => {
    let finish: ((value: typeof discussionReport) => void) | undefined;
    vi.mocked(api.qualifyProviderModel).mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    await openGateway();
    fireEvent.click(screen.getByTestId("gateway-qualify"));
    fireEvent.click(screen.getByTestId("gateway-qualify"));
    await waitFor(() => expect(api.upsertProviderProfile).toHaveBeenCalledTimes(1));
    expect(api.qualifyProviderModel).toHaveBeenCalledTimes(1);
    finish?.(discussionReport);
    expect(await screen.findByTestId("readiness-verdict")).toHaveTextContent(
      READINESS_VERDICT_LABEL.discussion_only,
    );
  });

  it("keeps environment-managed profiles read-only with understandable readiness", async () => {
    vi.mocked(api.settingsSnapshot).mockResolvedValue({
      model: "grok-4.5",
      effort: "none",
      gatewayProviderId: profile.id,
      gatewayProfiles: [
        {
          ...profile,
          managedByEnv: true,
          models: [readyModel],
        },
      ],
    });
    await openGateway();
    expect(screen.getByTestId("gateway-qualify")).toBeDisabled();
    expect(screen.getByTestId("gateway-save")).toBeDisabled();
    expect(screen.getByTestId("readiness-verdict")).toHaveTextContent(
      READINESS_VERDICT_LABEL.ready,
    );
    expect(screen.getByTestId("readiness-env")).toHaveTextContent(/environment/i);
    expect(screen.getByTestId("readiness-next-action-none")).toBeTruthy();
  });

  it("emphasizes credential repair for a measured model without a stored key", async () => {
    vi.mocked(api.settingsSnapshot).mockResolvedValue({
      model: "grok-4.5",
      effort: "none",
      gatewayProviderId: profile.id,
      gatewayProfiles: [
        {
          ...profile,
          credentialSet: false,
          models: [readyModel],
        },
      ],
    });
    await openGateway();
    expect(screen.getByTestId("readiness-verdict")).toHaveTextContent(
      READINESS_VERDICT_LABEL.credential_missing,
    );
    const next = screen.getByTestId("readiness-next-action");
    expect(next).toHaveTextContent("Add provider key");
    fireEvent.click(next);
    expect(document.activeElement).toBe(screen.getByTestId("gateway-api-key"));
  });

  it("keeps qualification errors out of credential placeholders and does not auto-retry", async () => {
    vi.mocked(api.qualifyProviderModel).mockRejectedValue(
      new Error("401 api_key=sk-secret-credential-value Saved (leave blank to keep)"),
    );
    await openGateway();
    fireEvent.click(screen.getByTestId("gateway-qualify"));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).not.toContain("sk-secret-credential-value");
    expect(alert.textContent).not.toContain("Saved (leave blank to keep)");
    expect(api.qualifyProviderModel).toHaveBeenCalledTimes(1);
  });
});
