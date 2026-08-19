import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { LaneScopeLine } from "./LaneScopeLine";

afterEach(cleanup);

describe("LaneScopeLine", () => {
  it("keeps all ownership facts visible on secondary surfaces", () => {
    render(
      <LaneScopeLine
        scope={{
          laneId: "lane-hosted",
          laneTitle: "Gateway qualification",
          agentLabel: "Release Warden",
          runtimeTarget: "hosted_service",
          runtimeConnection: "reconnecting",
          workspacePath: "/srv/grokptah/gateway",
          runLabel: "Queued · position 2",
        }}
      />,
    );

    const line = screen.getByRole("group", { name: "Lane scope" });
    expect(line).toHaveAttribute("data-lane-id", "lane-hosted");
    expect(line).toHaveTextContent("Lane Gateway qualification");
    expect(line).toHaveTextContent("Agent Release Warden");
    expect(line).toHaveTextContent("Runtime Hosted service · Reconnecting");
    expect(line).toHaveTextContent("Workspace srv / grokptah / gateway");
    expect(line).toHaveTextContent("Run Queued · position 2");
  });

  it("labels an unassigned Lane and missing workspace without exposing an id", () => {
    render(
      <LaneScopeLine
        scope={{ laneId: "lane-ad-hoc", laneTitle: "Scratch", workspacePath: null }}
        compact
      />,
    );

    const line = screen.getByRole("group", { name: "Lane scope" });
    expect(line).toHaveTextContent("Agent Ad hoc");
    expect(line).toHaveTextContent("Workspace No workspace selected");
    expect(line).not.toHaveTextContent("lane-ad-hoc");
  });
});
