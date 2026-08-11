import { describe, expect, it } from "vitest";
import type { AgentStatus } from "./protocol";
import { preserveProjectCwd } from "./projectCwd";

const status: AgentStatus = {
  running: true,
  project_cwd: null,
  active_session: null,
  always_approve: false,
  model: "grok-build",
  effort: "medium",
  sandbox_profile: "workspace-write",
  appearance: "dark",
  auto_update_enabled: false,
};

describe("preserveProjectCwd", () => {
  it("keeps a picker result visible when a refresh omits the path", () => {
    expect(preserveProjectCwd(status, "/tmp/demo").project_cwd).toBe("/tmp/demo");
  });

  it("does not replace an authoritative bridge path", () => {
    const current = { ...status, project_cwd: "/tmp/current" };
    expect(preserveProjectCwd(current, "/tmp/stale").project_cwd).toBe(
      "/tmp/current",
    );
  });
});
