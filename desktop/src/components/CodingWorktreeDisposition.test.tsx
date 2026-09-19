import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CodingWorktreeHostView } from "../lib/protocol";
import { CodingWorktreeDisposition } from "./CodingWorktreeDisposition";

const mocks = vi.hoisted(() => ({
  forSession: vi.fn(),
  pause: vi.fn(),
  stop: vi.fn(),
  accept: vi.fn(),
  discard: vi.fn(),
  keep: vi.fn(),
}));

vi.mock("../lib/api", () => ({
  api: {
    codingWorktreeForSession: mocks.forSession,
    codingWorktreePause: mocks.pause,
    codingWorktreeStop: mocks.stop,
    codingWorktreeAccept: mocks.accept,
    codingWorktreeDiscard: mocks.discard,
    codingWorktreeKeepForReview: mocks.keep,
  },
}));

function view(overrides: Partial<CodingWorktreeHostView> = {}): CodingWorktreeHostView {
  return {
    handle: "wcr-1",
    agentSessionId: "session-1",
    identity: {
      sessionId: "wcr-1",
      baseSha: "abc",
      worktreePathDigest: "sha256:wt",
      branchName: "wcr/wcr-1",
    },
    phase: "active",
    disposition: null,
    pauseFenced: false,
    settlementFenced: false,
    applyUncertain: false,
    worktreePath: "/tmp/wt",
    patchDigest: "sha256:patch",
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

beforeEach(() => {
  mocks.forSession.mockReset();
  mocks.pause.mockReset();
  mocks.stop.mockReset();
  mocks.accept.mockReset();
  mocks.discard.mockReset();
  mocks.keep.mockReset();
});

describe("CodingWorktreeDisposition", () => {
  it("does not look up or auto-accept when no AgentHost session is selected", () => {
    render(<CodingWorktreeDisposition sessionId={null} />);
    expect(screen.getByRole("region", { name: "Coding worktree disposition" })).toHaveAttribute(
      "data-status",
      "none",
    );
    expect(screen.getByText("No coding worktree session")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Accept" })).toBeNull();
    expect(mocks.forSession).not.toHaveBeenCalled();
    expect(mocks.accept).not.toHaveBeenCalled();
  });

  it("hides actions when the selected session has no coding worktree", async () => {
    mocks.forSession.mockResolvedValue(null);
    render(<CodingWorktreeDisposition sessionId="session-1" />);
    await waitFor(() => expect(mocks.forSession).toHaveBeenCalledWith("session-1"));
    expect(screen.getByText("No coding worktree session")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Pause" })).toBeNull();
    expect(mocks.accept).not.toHaveBeenCalled();
  });

  it("keeps Accept disabled until apply target is explicit and never auto-submits", async () => {
    mocks.forSession.mockResolvedValue(view());
    render(<CodingWorktreeDisposition sessionId="session-1" />);
    const accept = await screen.findByRole("button", { name: "Accept" });
    expect(accept).toBeDisabled();
    expect(screen.getByRole("button", { name: "Pause" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Stop" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Discard" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Keep for review" })).toBeEnabled();
    expect(mocks.accept).not.toHaveBeenCalled();

    fireEvent.change(screen.getByLabelText("Apply target"), {
      target: { value: "/tmp/apply" },
    });
    expect(accept).toBeEnabled();
    fireEvent.click(accept);
    await waitFor(() =>
      expect(mocks.accept).toHaveBeenCalledWith("wcr-1", "/tmp/apply", "sha256:patch"),
    );
  });

  it("disables settlement while Paused or Uncertain even with apply target filled", async () => {
    mocks.forSession.mockResolvedValue(
      view({ phase: "paused", pauseFenced: true }),
    );
    const paused = render(<CodingWorktreeDisposition sessionId="session-1" />);
    await screen.findByRole("button", { name: "Stop" });
    fireEvent.change(screen.getByLabelText("Apply target"), {
      target: { value: "/tmp/apply" },
    });
    expect(screen.getByDisplayValue("/tmp/apply")).toBeTruthy();
    expect(screen.getByDisplayValue("sha256:patch")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Pause" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Accept" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Discard" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Keep for review" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Stop" })).toBeEnabled();
    expect(mocks.accept).not.toHaveBeenCalled();
    paused.unmount();

    mocks.forSession.mockResolvedValue(
      view({
        phase: "settled",
        disposition: "uncertain",
        applyUncertain: true,
        pauseFenced: true,
        settlementFenced: true,
      }),
    );
    render(<CodingWorktreeDisposition sessionId="session-2" />);
    expect(await screen.findByRole("region")).toHaveAttribute("data-status", "uncertain");
    fireEvent.change(screen.getByLabelText("Apply target"), {
      target: { value: "/tmp/apply" },
    });
    expect(screen.getByDisplayValue("/tmp/apply")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Accept" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Stop" })).toBeEnabled();
    expect(mocks.accept).not.toHaveBeenCalled();
  });

  it("keeps Settled Accept off with a filled target and turns Stop off when Destroyed", async () => {
    mocks.forSession.mockResolvedValue(
      view({
        phase: "settled",
        disposition: "accepted",
        pauseFenced: true,
        settlementFenced: true,
      }),
    );
    const settled = render(<CodingWorktreeDisposition sessionId="session-1" />);
    await screen.findByRole("button", { name: "Stop" });
    fireEvent.change(screen.getByLabelText("Apply target"), {
      target: { value: "/tmp/apply" },
    });
    expect(screen.getByRole("region")).toHaveAttribute("data-status", "settled");
    expect(screen.getByRole("button", { name: "Accept" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Pause" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Discard" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Keep for review" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Stop" })).toBeEnabled();
    settled.unmount();

    mocks.forSession.mockResolvedValue(
      view({
        phase: "destroyed",
        disposition: "stopped",
        pauseFenced: true,
        settlementFenced: true,
      }),
    );
    render(<CodingWorktreeDisposition sessionId="session-2" />);
    expect(await screen.findByRole("region")).toHaveAttribute("data-status", "destroyed");
    expect(screen.getByRole("button", { name: "Stop" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Accept" })).toBeDisabled();
  });

  it("does not paint a prior-session mutation onto a newly selected session", async () => {
    let resolvePause: (value: CodingWorktreeHostView) => void = () => {};
    mocks.forSession.mockResolvedValueOnce(view());
    mocks.pause.mockImplementationOnce(
      () =>
        new Promise<CodingWorktreeHostView>((resolve) => {
          resolvePause = resolve;
        }),
    );
    const ui = render(<CodingWorktreeDisposition sessionId="session-1" />);
    fireEvent.click(await screen.findByRole("button", { name: "Pause" }));
    await waitFor(() => expect(mocks.pause).toHaveBeenCalledWith("wcr-1"));

    mocks.forSession.mockResolvedValueOnce(null);
    ui.rerender(<CodingWorktreeDisposition sessionId="session-2" />);
    await waitFor(() => expect(mocks.forSession).toHaveBeenCalledWith("session-2"));
    expect(screen.getByText("No coding worktree session")).toBeTruthy();

    await act(async () => {
      resolvePause(view({ handle: "wcr-1", phase: "paused", pauseFenced: true }));
    });
    expect(screen.queryByText("wcr-1")).toBeNull();
    expect(screen.getByText("No coding worktree session")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
  });

  it("surfaces host errors honestly without retrying Accept", async () => {
    mocks.forSession.mockResolvedValue(view());
    mocks.pause.mockRejectedValue("UncertainOutcome: pause forbidden while disposition is Uncertain");
    render(<CodingWorktreeDisposition sessionId="session-1" />);
    fireEvent.click(await screen.findByRole("button", { name: "Pause" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("No auto-retry was attempted.");
    fireEvent.click(screen.getByText("Technical details"));
    expect(screen.getByText("UncertainOutcome: pause forbidden while disposition is Uncertain")).toBeTruthy();
    expect(mocks.accept).not.toHaveBeenCalled();
  });
});
