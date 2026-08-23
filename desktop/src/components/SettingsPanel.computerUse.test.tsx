import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SettingsPanel } from "./SettingsPanel";
import type {
  ComputerIsolatedVisualStatus,
  ComputerObservationPreview,
  ComputerPlatformStatus,
  ComputerTargetCandidate,
} from "../lib/protocol";

const mocks = vi.hoisted(() => ({
  settingsSnapshot: vi.fn(),
  status: vi.fn(),
  isolatedStatus: vi.fn(),
  requestPermission: vi.fn(),
  listTargets: vi.fn(),
  observeOnce: vi.fn(),
}));

vi.mock("../lib/api", () => ({
  api: {
    settingsSnapshot: mocks.settingsSnapshot,
    computerUseStatus: mocks.status,
    computerUseIsolatedVisualStatus: mocks.isolatedStatus,
    computerUseRequestPermission: mocks.requestPermission,
    computerUseListTargets: mocks.listTargets,
    computerUseObserveOnce: mocks.observeOnce,
  },
}));

const missing: ComputerPlatformStatus = {
  platformId: "macos",
  available: true,
  minimumOsVersion: "14.0",
  screenRecording: "missing",
  accessibility: "missing",
};

const granted: ComputerPlatformStatus = {
  ...missing,
  screenRecording: "granted",
  accessibility: "granted",
};

const isolatedPending: ComputerIsolatedVisualStatus = {
  platformId: "macos",
  available: false,
  hostCapable: true,
  minimumOsVersion: "14.0",
  operatingSystemSupported: true,
  virtualizationFrameworkAvailable: true,
  helperVirtualizationEntitlementVerified: false,
  backendPackaged: false,
  guestImageMeasured: false,
  launchAttempted: false,
  blocker: "backend_not_packaged",
  detail:
    "Host virtualization is ready; the signed helper and measured guest are not packaged yet",
};

const candidate: ComputerTargetCandidate = {
  selectionToken: "selection-token",
  target: {
    appId: "com.example.demo",
    windowId: "macos-window-1",
    generation: 1,
    displayName: "Demo App",
    sensitivity: "none",
  },
  geometry: { x: 0, y: 0, width: 640, height: 480, scaleFactor: 2 },
  onScreen: true,
  active: true,
  minimized: false,
};

const preview: ComputerObservationPreview = {
  observation: {
    observationId: "observation-1",
    sequence: 1,
    target: candidate.target,
    elements: [],
    elementsTruncated: false,
    screenshot: {
      contentSha256: "a".repeat(64),
      byteLen: 10,
      width: 640,
      height: 480,
      redacted: true,
      assetId: "asset-1",
    },
  },
  imageDataUrl: "data:image/png;base64,AA==",
};

function settingsPanel(open = true) {
  return (
    <SettingsPanel
      open={open}
      onClose={vi.fn()}
      models={[]}
      auth={{ signed_in: false }}
      onAuthChange={vi.fn()}
      onChromeChange={vi.fn()}
    />
  );
}

function renderSettings() {
  return render(settingsPanel());
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.settingsSnapshot.mockResolvedValue({});
  mocks.status.mockResolvedValue(missing);
  mocks.isolatedStatus.mockResolvedValue(isolatedPending);
  mocks.requestPermission.mockResolvedValue("prompt_pending");
  mocks.listTargets.mockResolvedValue([candidate]);
  mocks.observeOnce.mockResolvedValue(preview);
});

afterEach(cleanup);

describe("Computer Use settings", () => {
  it("never requests consent or captures on open", async () => {
    renderSettings();
    fireEvent.click(screen.getByText("Computer Use"));
    await waitFor(() => expect(mocks.status).toHaveBeenCalled());
    expect(mocks.isolatedStatus).toHaveBeenCalledTimes(1);

    expect(mocks.requestPermission).not.toHaveBeenCalled();
    expect(mocks.listTargets).not.toHaveBeenCalled();
    expect(mocks.observeOnce).not.toHaveBeenCalled();
    expect(screen.getByText("Isolated visual workspace")).toBeTruthy();
    expect(screen.getByText("Reserved for signed helper")).toBeTruthy();
    expect(screen.getByText(/did not launch a virtual machine/)).toBeTruthy();
    expect(
      screen.getByText(/Codex Computer Use and Terminal grants do not grant GrokPtah access/),
    ).toBeTruthy();

    fireEvent.click(screen.getAllByText("Request")[0]);
    await waitFor(() =>
      expect(mocks.requestPermission).toHaveBeenCalledWith("screen_recording"),
    );
  });

  it("requires separate find and observe clicks", async () => {
    mocks.status.mockResolvedValue(granted);
    renderSettings();
    fireEvent.click(screen.getByText("Computer Use"));
    await waitFor(() => expect(screen.getByText("Find windows")).toBeEnabled());

    expect(mocks.listTargets).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText("Find windows"));
    await waitFor(() => expect(screen.getByText("Demo App")).toBeTruthy());
    expect(mocks.observeOnce).not.toHaveBeenCalled();

    fireEvent.click(screen.getByText("Observe once"));
    await waitFor(() =>
      expect(mocks.observeOnce).toHaveBeenCalledWith("selection-token"),
    );
    expect(
      screen.getByAltText("Redacted observation of Demo App"),
    ).toBeTruthy();
  });

  it("discards an observation that finishes after the panel closes", async () => {
    mocks.status.mockResolvedValue(granted);
    let resolveObservation: (
      value: ComputerObservationPreview,
    ) => void = () => {};
    mocks.observeOnce.mockImplementation(
      () =>
        new Promise<ComputerObservationPreview>((resolve) => {
          resolveObservation = resolve;
        }),
    );
    const view = renderSettings();
    fireEvent.click(screen.getByText("Computer Use"));
    await waitFor(() => expect(screen.getByText("Find windows")).toBeEnabled());
    fireEvent.click(screen.getByText("Find windows"));
    await waitFor(() => expect(screen.getByText("Demo App")).toBeTruthy());
    fireEvent.click(screen.getByText("Observe once"));
    await waitFor(() => expect(mocks.observeOnce).toHaveBeenCalled());

    view.rerender(settingsPanel(false));
    await act(async () => resolveObservation(preview));
    view.rerender(settingsPanel());

    expect(screen.getByRole("heading", { name: "Computer Use" })).toBeTruthy();
    expect(
      screen.queryByAltText("Redacted observation of Demo App"),
    ).toBeNull();
  });
});
