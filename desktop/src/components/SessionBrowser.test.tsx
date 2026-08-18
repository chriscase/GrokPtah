import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "../lib/protocol";
import { SessionBrowser } from "./SessionBrowser";

const apiMocks = vi.hoisted(() => ({
  sessionListAll: vi.fn(),
  sessionListArchived: vi.fn(),
  sessionListByKind: vi.fn(),
  sessionListFolders: vi.fn(),
  sessionListTags: vi.fn(),
  sessionArchive: vi.fn(),
  sessionRename: vi.fn(),
  sessionDelete: vi.fn(),
  sessionSetFolder: vi.fn(),
  sessionSetTags: vi.fn(),
}));

vi.mock("../lib/api", () => ({ api: apiMocks }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function archivedLane(): SessionSummary {
  return {
    id: "lane-archived",
    title: "Previous gateway experiment",
    cwd: "/work/grokptah",
    created_at: "2026-08-17T00:00:00Z",
    updated_at: "2026-08-18T00:00:00Z",
    message_count: 12,
    archived: true,
    kind: "build",
  };
}

describe("SessionBrowser archived Lane boundary", () => {
  it("inspects without restoring and provides a separate Restore action", async () => {
    const lane = archivedLane();
    apiMocks.sessionListAll.mockResolvedValue([]);
    apiMocks.sessionListArchived.mockResolvedValue([lane]);
    apiMocks.sessionListFolders.mockResolvedValue([]);
    apiMocks.sessionListTags.mockResolvedValue([]);
    apiMocks.sessionArchive.mockResolvedValue({ ...lane, archived: false });
    const onOpen = vi.fn();
    const onChanged = vi.fn();

    render(
      <SessionBrowser
        open
        activeSessionId={null}
        onClose={vi.fn()}
        onOpen={onOpen}
        onChanged={onChanged}
      />,
    );

    await screen.findByText("No Lanes match this view");
    fireEvent.click(screen.getByRole("button", { name: "Archive" }));
    const inspect = await screen.findByRole("button", { name: "Inspect" });
    fireEvent.click(inspect);
    expect(onOpen).toHaveBeenCalledWith(lane);
    expect(apiMocks.sessionArchive).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Restore" }));
    await waitFor(() => {
      expect(apiMocks.sessionArchive).toHaveBeenCalledWith(lane.id, false);
    });
    expect(onChanged).toHaveBeenCalled();
  });
});
