import { describe, expect, it } from "vitest";
import { findTabOfKind, kindForTab } from "./sessionTab";
import type { SessionTab } from "./protocol";

const buildTab: SessionTab = {
  id: "build-1",
  title: "Build",
  kind: "build",
  cwd: "/tmp/demo",
  transcript: [],
  busy: false,
  plan: null,
  activity: {
    phase: "idle",
    label: "Idle",
    live: false,
    lastEventAt: 0,
  },
  unseen: false,
  needsPermission: false,
  completionEvidence: null,
};

describe("session tab identity", () => {
  it("keeps a hidden Build tab identified as Build", () => {
    expect(kindForTab(buildTab, [], "chat")).toBe("build");
    expect(findTabOfKind([buildTab], "build")).toBe(buildTab);
  });

  it("uses the filtered session list only when tab metadata is unavailable", () => {
    const legacyTab = { ...buildTab, kind: undefined } as unknown as SessionTab;
    expect(
      kindForTab(
        legacyTab,
        [
          {
            id: buildTab.id,
            title: buildTab.title,
            cwd: buildTab.cwd ?? "",
            created_at: "",
            updated_at: "",
            message_count: 0,
            kind: "chat",
          },
        ],
        "build",
      ),
    ).toBe("chat");
  });
});
