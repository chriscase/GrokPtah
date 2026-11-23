import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { LaneSummary, RuntimeConnectionState, RuntimeTarget } from "../lib/protocol";
import {
  LaneContextHeader,
  runtimeConnectionLabel,
  runtimeTargetLabel,
  workspaceDisplayName,
} from "./LaneContextHeader";

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

function header(): HTMLElement {
  const section = document.querySelector("section.lane-context-header");
  if (!(section instanceof HTMLElement)) throw new Error("header section not rendered");
  return section;
}

function fact(term: string): { dt: HTMLElement; dd: HTMLElement } {
  const dt = within(header())
    .getAllByText(term)
    .find((el) => el.tagName === "DT");
  if (!dt) throw new Error(`no <dt> named ${term}`);
  const dd = dt.nextElementSibling;
  if (!(dd instanceof HTMLElement) || dd.tagName !== "DD") {
    throw new Error(`no <dd> after ${term}`);
  }
  return { dt, dd };
}

describe("runtime label helpers", () => {
  it("maps every runtime target and defaults to the local desktop", () => {
    const cases: Array<[RuntimeTarget | undefined, string]> = [
      ["local_desktop", "Local desktop"],
      ["local_service", "Local service / VM"],
      ["hosted_service", "Hosted service"],
      [undefined, "Local desktop"],
    ];
    for (const [value, label] of cases) {
      expect(runtimeTargetLabel(value)).toBe(label);
    }
  });

  it("maps every connection state and defaults to Connected", () => {
    const cases: Array<[RuntimeConnectionState | undefined, string]> = [
      ["connected", "Connected"],
      ["reconnecting", "Reconnecting"],
      ["disconnected", "Disconnected"],
      ["error", "Unavailable"],
      [undefined, "Connected"],
    ];
    for (const [value, label] of cases) {
      expect(runtimeConnectionLabel(value)).toBe(label);
    }
  });

  it("shortens Unix and Windows paths the same way and names the missing case", () => {
    expect(workspaceDisplayName(null)).toBe("No workspace selected");
    expect(workspaceDisplayName(undefined)).toBe("No workspace selected");
    expect(workspaceDisplayName("")).toBe("No workspace selected");
    expect(workspaceDisplayName("/work/grokptah/crates/codegen")).toBe(
      "grokptah / crates / codegen",
    );
    expect(workspaceDisplayName("C:\\Users\\dev\\repos\\grokptah")).toBe(
      "dev / repos / grokptah",
    );
    expect(workspaceDisplayName("/repo")).toBe("repo");
    expect(workspaceDisplayName("/a/b/")).toBe("a / b");
  });
});

describe("LaneContextHeader orientation", () => {
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

    expect(
      screen.getByRole("region", { name: "Stabilize retry queue" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { level: 1, name: "Stabilize retry queue" }),
    ).toBeInTheDocument();
    expect(fact("Agent").dd).toHaveTextContent("Ptah Refactorer");
    expect(fact("Runtime").dd).toHaveTextContent("Local desktop · Connected");
    expect(fact("Workspace").dd).toHaveTextContent("grokptah / crates / codegen");
    expect(fact("Run").dd).toHaveTextContent("Working");
  });

  it("labels the selected Lane distinctly from any Run Lane", () => {
    render(
      <LaneContextHeader lane={lane()} fallbackTitle="Fallback" runLabel="Idle" runLive={false} />,
    );
    expect(screen.getByText("Selected Lane")).toBeInTheDocument();
    expect(screen.queryByText("Run Lane")).toBeNull();
  });

  it("keeps a stable heading identity while operational state changes", () => {
    const view = render(
      <LaneContextHeader lane={lane()} fallbackTitle="Fallback" runLabel="Idle" runLive={false} />,
    );
    const heading = screen.getByRole("heading", { name: "Stabilize retry queue" });
    const id = heading.id;
    expect(id).toBe("lane-context-lane-local");
    expect(header()).toHaveAttribute("data-state", "ready");

    view.rerender(
      <LaneContextHeader lane={lane()} fallbackTitle="Fallback" runLabel="Working" runLive />,
    );
    expect(header()).toHaveAttribute("data-state", "running");
    expect(screen.getByRole("heading", { name: "Stabilize retry queue" }).id).toBe(id);
  });

  it("falls back cleanly when no Lane is selected", () => {
    render(
      <LaneContextHeader lane={null} fallbackTitle="Session tab title" runLabel="Idle" runLive={false} />,
    );

    expect(
      screen.getByRole("heading", { level: 1, name: "Session tab title" }),
    ).toHaveAttribute("id", "lane-context-current");
    expect(header()).not.toHaveAttribute("data-lane-id");
    expect(header()).not.toHaveAttribute("data-run-lane-id");
    expect(fact("Agent").dd).toHaveTextContent("Ad hoc");
    expect(fact("Runtime").dd).toHaveTextContent("Local desktop · Connected");
    expect(fact("Workspace").dd).toHaveTextContent("No workspace selected");
  });

  it("uses the final fallback title when everything else is empty", () => {
    render(<LaneContextHeader lane={null} fallbackTitle="" runLabel="Idle" runLive={false} />);
    expect(screen.getByRole("heading", { level: 1, name: "Current work" })).toBeInTheDocument();
  });

  it("preserves full evidence in titles when text may be visually shortened", () => {
    const longTitle = "Re-platform the entire ingestion pipeline onto the durable manager v2 model";
    const longPath =
      "/Users/dev/very/long/nested/workspace/path/that/keeps/going/grokptah/crates/codegen";
    const longAgent = "Ptah Long-Horizon Autonomous Refactoring Agent (durable, v2)";
    const longRun = "Applying migration 47 of 512 — rewriting call sites in crates/codegen";
    render(
      <LaneContextHeader
        lane={lane({ title: longTitle, cwd: longPath })}
        fallbackTitle="Fallback"
        agentId={longAgent}
        runLabel={longRun}
        runLive
      />,
    );

    expect(screen.getByRole("heading", { level: 1 })).toHaveAttribute("title", longTitle);
    expect(fact("Workspace").dd).toHaveAttribute("title", longPath);
    expect(fact("Agent").dd).toHaveAttribute("title", longAgent);
    expect(fact("Run").dd).toHaveAttribute("title", longRun);
  });

  it("displays a Windows workspace path without misleading truncation", () => {
    render(
      <LaneContextHeader
        lane={lane({ cwd: "C:\\Users\\dev\\repos\\grokptah" })}
        fallbackTitle="Fallback"
        runLabel="Idle"
        runLive={false}
      />,
    );
    const { dd } = fact("Workspace");
    expect(dd).toHaveTextContent("dev / repos / grokptah");
    expect(dd).toHaveAttribute("title", "C:\\Users\\dev\\repos\\grokptah");
  });
});

describe("operational state priority", () => {
  const connectionStates: Array<{
    connection: RuntimeConnectionState;
    label: string;
    state: string;
  }> = [
    { connection: "error", label: "Unavailable", state: "error" },
    { connection: "disconnected", label: "Disconnected", state: "disconnected" },
    { connection: "reconnecting", label: "Reconnecting", state: "reconnecting" },
  ];

  it("shows Ready when idle and connected", () => {
    render(
      <LaneContextHeader lane={lane()} fallbackTitle="Fallback" runLabel="Idle" runLive={false} />,
    );
    expect(screen.getByRole("status")).toHaveTextContent("Ready");
    expect(header()).toHaveAttribute("data-state", "ready");
  });

  it("shows Running while a Run is live on a healthy connection", () => {
    render(
      <LaneContextHeader lane={lane()} fallbackTitle="Fallback" runLabel="Working" runLive />,
    );
    expect(screen.getByRole("status")).toHaveTextContent("Running");
    expect(header()).toHaveAttribute("data-state", "running");
  });

  for (const { connection, label, state } of connectionStates) {
    it(`prioritizes ${label} over a live Run`, () => {
      render(
        <LaneContextHeader
          lane={lane({ runtime_connection: connection })}
          fallbackTitle="Fallback"
          runLabel="Working"
          runLive
        />,
      );
      expect(screen.getByRole("status")).toHaveTextContent(label);
      expect(header()).toHaveAttribute("data-state", state);
    });
  }

  it("gates the operational state on the Run Lane connection when a remote target is active", () => {
    render(
      <LaneContextHeader
        lane={lane({ runtime_connection: "connected" })}
        fallbackTitle="Fallback"
        runLane={lane({
          id: "lane-hosted",
          session_id: "lane-hosted",
          runtime_target: "hosted_service",
          runtime_connection: "reconnecting",
        })}
        runLabel="Working"
        runLive
      />,
    );
    expect(header()).toHaveAttribute("data-state", "reconnecting");
    expect(fact("Runtime").dd).toHaveTextContent("Local desktop · Connected");
  });

  it("prioritizes Archived over every runtime signal", () => {
    render(
      <LaneContextHeader
        lane={lane({ archived: true, runtime_connection: "error" })}
        fallbackTitle="Fallback"
        runLabel="Working"
        runLive
      />,
    );
    expect(screen.getByRole("status")).toHaveTextContent("Archived");
    expect(header()).toHaveAttribute("data-state", "archived");
  });

  it("states the condition in text with a redundant non-color mark", () => {
    render(
      <LaneContextHeader
        lane={lane({ runtime_connection: "disconnected" })}
        fallbackTitle="Fallback"
        runLabel="Idle"
        runLive={false}
      />,
    );
    const pill = screen.getByRole("status");
    expect(pill).toHaveTextContent("Disconnected");
    expect(pill).toHaveAttribute("data-state", "disconnected");
    const mark = pill.querySelector(".lane-context-state-mark");
    expect(mark).not.toBeNull();
    expect(mark).toHaveAttribute("aria-hidden");
  });
});

describe("selected Lane vs remote Run Lane", () => {
  function renderRemote(runLaneOverrides: Partial<LaneSummary> = {}) {
    return render(
      <LaneContextHeader
        lane={lane()}
        fallbackTitle="Fallback"
        runLane={lane({
          id: "lane-hosted",
          session_id: "lane-hosted",
          title: "Gateway qualification",
          cwd: "/srv/hosted/gateway",
          runtime_target: "hosted_service",
          runtime_connection: "connected",
          ...runLaneOverrides,
        })}
        runLabel="Queued"
        runLive
      />,
    );
  }

  it("keeps the remote execution target separate from selected-Lane ownership", () => {
    renderRemote();

    expect(fact("Runtime").dd).toHaveTextContent("Local desktop · Connected");
    const runLaneFact = fact("Run Lane");
    expect(runLaneFact.dd).toHaveTextContent("Gateway qualification");
    expect(runLaneFact.dd).toHaveTextContent("Hosted service");
    expect(runLaneFact.dd).toHaveTextContent("Connected");
    expect(header()).toHaveAttribute("data-lane-id", "lane-local");
    expect(header()).toHaveAttribute("data-run-lane-id", "lane-hosted");
  });

  it("marks the Run Lane row as explicitly remote and subordinate", () => {
    renderRemote();
    const group = header().querySelector(".lane-context-run-lane");
    expect(group).not.toBeNull();
    expect(group).toHaveAttribute("data-run-lane-target", "hosted_service");
    expect(group).toHaveAttribute("data-run-lane-connection", "connected");
    expect(within(group as HTMLElement).getByText("Remote")).toBeInTheDocument();
    expect(fact("Run Lane").dt.textContent).toContain("Run Lane");
  });

  it("carries full Run Lane evidence in the title even when text is shortened", () => {
    renderRemote({ runtime_connection: "reconnecting" });
    const { dd } = fact("Run Lane");
    expect(dd).toHaveAttribute(
      "title",
      "Gateway qualification · Hosted service · Reconnecting · /srv/hosted/gateway",
    );
  });

  it("names an untitled Run Lane by its workspace", () => {
    renderRemote({ title: "" });
    expect(fact("Run Lane").dd).toHaveTextContent("srv / hosted / gateway");
  });

  it("shows no Run Lane row when the Run executes on the selected Lane", () => {
    render(
      <LaneContextHeader
        lane={lane()}
        fallbackTitle="Fallback"
        runLane={lane()}
        runLabel="Working"
        runLive
      />,
    );
    expect(screen.queryByText("Run Lane")).toBeNull();
    expect(header()).toHaveAttribute("data-run-lane-id", "lane-local");
  });

  it("attributes the Run to the selected Lane when no remote target exists", () => {
    render(
      <LaneContextHeader lane={lane()} fallbackTitle="Fallback" runLabel="Working" runLive />,
    );
    expect(screen.queryByText("Run Lane")).toBeNull();
    expect(header()).toHaveAttribute("data-run-lane-id", "lane-local");
  });
});

describe("workspace and archival actions", () => {
  it("offers Change workspace only when allowed and reports the intent accessibly", () => {
    const onChangeWorkspace = vi.fn();
    render(
      <LaneContextHeader
        lane={lane()}
        fallbackTitle="Fallback"
        runLabel="Idle"
        runLive={false}
        onChangeWorkspace={onChangeWorkspace}
      />,
    );
    const button = screen.getByRole("button", { name: "Change workspace" });
    fireEvent.click(button);
    expect(onChangeWorkspace).toHaveBeenCalledOnce();
  });

  it("hides Change workspace when no handler is provided", () => {
    render(
      <LaneContextHeader lane={lane()} fallbackTitle="Fallback" runLabel="Idle" runLive={false} />,
    );
    expect(screen.queryByRole("button", { name: "Change workspace" })).toBeNull();
  });

  it("makes archived inspection read-only until an explicit restore", () => {
    const onRestore = vi.fn();
    const onChangeWorkspace = vi.fn();
    render(
      <LaneContextHeader
        lane={lane({ archived: true })}
        fallbackTitle="Fallback"
        runLabel="Idle"
        runLive={false}
        onRestore={onRestore}
        onChangeWorkspace={onChangeWorkspace}
      />,
    );

    expect(screen.getByText("Archived Lane — inspection only")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Change workspace" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Restore Lane" }));
    expect(onRestore).toHaveBeenCalledOnce();
    expect(onChangeWorkspace).not.toHaveBeenCalled();
  });

  it("shows no Restore action when restoring is not offered", () => {
    render(
      <LaneContextHeader
        lane={lane({ archived: true })}
        fallbackTitle="Fallback"
        runLabel="Idle"
        runLive={false}
      />,
    );
    expect(screen.getByText("Archived Lane — inspection only")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Restore Lane" })).toBeNull();
  });

  it("surfaces a blocked workspace as an alert without mutating ownership facts", () => {
    render(
      <LaneContextHeader
        lane={lane({ workspace_status: "missing" })}
        fallbackTitle="Fallback"
        agentId="Ptah Refactorer"
        runLabel="Workspace blocked"
        runLive={false}
        scopeError="Lane workspace is missing; choose a valid workspace before resuming work"
      />,
    );

    expect(screen.getByText("Lane workspace unavailable")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(/Lane workspace is missing/);
    expect(header()).toHaveAttribute("data-lane-id", "lane-local");
    expect(header()).toHaveAttribute("data-state", "ready");
    expect(fact("Agent").dd).toHaveTextContent("Ptah Refactorer");
    expect(fact("Runtime").dd).toHaveTextContent("Local desktop · Connected");
  });

  it("suppresses the blocked card while archived so restore stays the one action", () => {
    render(
      <LaneContextHeader
        lane={lane({ archived: true })}
        fallbackTitle="Fallback"
        runLabel="Idle"
        runLive={false}
        scopeError="Lane workspace is missing"
      />,
    );
    expect(screen.queryByText("Lane workspace unavailable")).toBeNull();
    expect(screen.getByText("Archived Lane — inspection only")).toBeInTheDocument();
  });
});

describe("structural invariants for layout safety", () => {
  it("keeps the class hooks the responsive stylesheet depends on", () => {
    render(
      <LaneContextHeader
        lane={lane()}
        fallbackTitle="Fallback"
        runLane={lane({ id: "lane-hosted", session_id: "lane-hosted" })}
        runLabel="Working"
        runLive
      />,
    );

    const section = header();
    expect(section.classList.contains("lane-context-header")).toBe(true);
    const facts = section.querySelector("dl.lane-context-facts");
    expect(facts).not.toBeNull();
    expect(facts?.querySelectorAll(".lane-context-fact").length).toBeGreaterThanOrEqual(4);
    expect(section.querySelector(".lane-context-workspace")).not.toBeNull();
    expect(section.querySelector(".lane-context-run-lane")).not.toBeNull();
    expect(section.querySelector(".lane-context-heading-row")).not.toBeNull();
  });

  it("uses definition-list semantics for the facts", () => {
    render(
      <LaneContextHeader lane={lane()} fallbackTitle="Fallback" runLabel="Idle" runLive={false} />,
    );
    for (const term of ["Agent", "Runtime", "Workspace", "Run"]) {
      const { dt, dd } = fact(term);
      expect(dt.tagName).toBe("DT");
      expect(dd.tagName).toBe("DD");
    }
  });
});
