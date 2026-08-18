import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { LaneSummary } from "../lib/protocol";
import { LaneContextHeader } from "./LaneContextHeader";

afterEach(cleanup);

function lane(overrides: Partial<LaneSummary> = {}): LaneSummary {
  return {
    id: "lane-local",
    session_id: "lane-local",
    agent_id: "Ptah Refactorer",
    title: "Stabilize retry queue",
    cwd: "/work/grokptah/crates/codegen",
    created_at: "2026-08-18T00:00:00Z",
    updated_at: "2026-08-18T00:00:00Z",
    message_count: 3,
    runtime_target: "local_desktop",
    runtime_connection: "connected",
    workspace_status: "ready",
    ...overrides,
  };
}

describe("LaneContextHeader", () => {
  it("names the selected Lane, Agent, Runtime, workspace, and Run", () => {
    render(
      <LaneContextHeader
        lane={lane()}
        fallbackTitle="Fallback"
        agentId="Ptah Refactorer"
        runLabel="Working"
        runLive
      />,
    );

    expect(screen.getByRole("heading", { name: "Stabilize retry queue" })).toBeTruthy();
    expect(screen.getByText("Ptah Refactorer")).toBeTruthy();
    expect(screen.getByText(/Local desktop.*Connected/)).toBeTruthy();
    expect(screen.getByText("grokptah / crates / codegen")).toBeTruthy();
    expect(screen.getByText("Working")).toBeTruthy();
  });

  it("keeps a remote execution target visibly separate from the selected Lane", () => {
    render(
      <LaneContextHeader
        lane={lane()}
        fallbackTitle="Fallback"
        runLane={lane({
          id: "lane-hosted",
          session_id: "lane-hosted",
          title: "Gateway qualification",
          runtime_target: "hosted_service",
        })}
        runLabel="Queued"
        runLive
      />,
    );

    const runLane = screen.getByText("Run Lane").nextElementSibling;
    expect(screen.getByText("Runtime").nextElementSibling?.textContent).toContain(
      "Local desktop",
    );
    expect(runLane?.textContent).toContain("Gateway qualification");
    expect(runLane?.textContent).toContain("Hosted service");
    expect(runLane.closest("section")).toHaveAttribute(
      "data-lane-id",
      "lane-local",
    );
    expect(runLane.closest("section")).toHaveAttribute(
      "data-run-lane-id",
      "lane-hosted",
    );
  });

  it("makes archived inspection read-only until an explicit restore", () => {
    const onRestore = vi.fn();
    const onChangeWorkspace = vi.fn();
    render(
      <LaneContextHeader
        lane={lane({ archived: true })}
        fallbackTitle="Fallback"
        runLabel="Ready"
        runLive={false}
        onRestore={onRestore}
        onChangeWorkspace={onChangeWorkspace}
      />,
    );

    expect(screen.getByText("Archived Lane — inspection only")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Change" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Restore Lane" }));
    expect(onRestore).toHaveBeenCalledOnce();
    expect(onChangeWorkspace).not.toHaveBeenCalled();
  });

  it("surfaces a blocked workspace without changing ownership", () => {
    render(
      <LaneContextHeader
        lane={lane({ workspace_status: "missing" })}
        fallbackTitle="Fallback"
        runLabel="Workspace blocked"
        runLive={false}
        scopeError="Lane workspace is missing; choose a valid workspace before resuming work"
      />,
    );

    expect(screen.getByText("Lane workspace unavailable")).toBeTruthy();
    expect(screen.getByRole("alert")).toBeTruthy();
    expect(screen.getByText(/Lane workspace is missing/)).toBeTruthy();
  });
});
