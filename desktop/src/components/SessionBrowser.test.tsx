import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "../lib/protocol";
import { SessionBrowser, type SessionBrowserProps } from "./SessionBrowser";

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

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (v: T) => void;
  reject: (e: unknown) => void;
};

function deferred<T>(): Deferred<T> {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

let laneSeq = 0;
beforeEach(() => {
  laneSeq = 0;
});

function lane(overrides: Partial<SessionSummary> = {}): SessionSummary {
  laneSeq += 1;
  return {
    id: `lane-${laneSeq}`,
    title: `Lane ${laneSeq}`,
    cwd: "/work/grokptah",
    created_at: "2026-08-17T00:00:00Z",
    updated_at: "2026-08-18T00:00:00Z",
    message_count: 3,
    archived: false,
    kind: "build",
    ...overrides,
  };
}

/** Happy-path list mocks; ops resolve so refresh-after-op never dangles. */
function mockLists(
  rows: SessionSummary[],
  opts: { folders?: string[]; tags?: string[] } = {},
) {
  apiMocks.sessionListAll.mockResolvedValue(rows);
  apiMocks.sessionListArchived.mockResolvedValue(
    rows.filter((r) => r.archived),
  );
  apiMocks.sessionListByKind.mockImplementation((kind: string) =>
    Promise.resolve(rows.filter((r) => (r.kind ?? "build") === kind)),
  );
  apiMocks.sessionListFolders.mockResolvedValue(opts.folders ?? []);
  apiMocks.sessionListTags.mockResolvedValue(opts.tags ?? []);
  apiMocks.sessionArchive.mockResolvedValue(undefined);
  apiMocks.sessionRename.mockResolvedValue(undefined);
  apiMocks.sessionDelete.mockResolvedValue(undefined);
  apiMocks.sessionSetFolder.mockResolvedValue(undefined);
  apiMocks.sessionSetTags.mockResolvedValue(undefined);
}

function renderBrowser(props: Partial<SessionBrowserProps> = {}) {
  const onClose = vi.fn();
  const onOpen = vi.fn();
  const onChanged = vi.fn();
  const utils = render(
    <SessionBrowser
      open
      activeSessionId={null}
      onClose={onClose}
      onOpen={onOpen}
      onChanged={onChanged}
      {...props}
    />,
  );
  return { ...utils, onClose, onOpen, onChanged };
}

function rowFor(title: string): HTMLElement {
  const row = screen.getByText(title).closest("li");
  if (!row) throw new Error(`no row for ${title}`);
  return row;
}

function filterInput(): HTMLElement {
  return screen.getByRole("textbox", { name: "Filter Lanes" });
}

describe("SessionBrowser data lifecycle", () => {
  it("shows loading, then the populated list with a visible count", async () => {
    const first = lane({ title: "Alpha" });
    const second = lane({ title: "Beta" });
    mockLists([first, second]);
    const d = deferred<SessionSummary[]>();
    apiMocks.sessionListAll.mockReturnValueOnce(d.promise);

    renderBrowser();
    expect(screen.getByText("Loading Lanes")).toBeInTheDocument();

    await act(async () => {
      d.resolve([first, second]);
    });
    await screen.findByText("Alpha");
    expect(screen.getByText("Beta")).toBeInTheDocument();
    expect(screen.getByText("2 Lanes")).toBeInTheDocument();
    expect(screen.queryByText("Loading Lanes")).toBeNull();
  });

  it("shows the empty state when nothing matches", async () => {
    mockLists([]);
    renderBrowser();
    await screen.findByText("No Lanes match this view");
    expect(screen.getByText("0 Lanes")).toBeInTheDocument();
  });

  it("surfaces load errors with a working retry", async () => {
    const only = lane({ title: "Recovered" });
    mockLists([only]);
    apiMocks.sessionListAll.mockRejectedValueOnce("backend down");

    renderBrowser();
    await screen.findByText("Lanes could not be loaded");
    expect(screen.getByText("backend down")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByText("Recovered");
    expect(screen.queryByText("Lanes could not be loaded")).toBeNull();
  });

  it("drops a stale in-flight load when the scope changes", async () => {
    const activeLane = lane({ title: "Beta Active" });
    const archivedLane = lane({ title: "Alpha Archived", archived: true });
    mockLists([archivedLane]);
    const stale = deferred<SessionSummary[]>();
    apiMocks.sessionListAll.mockReturnValueOnce(stale.promise);

    renderBrowser();
    fireEvent.click(screen.getByRole("button", { name: "Archive" }));
    await screen.findByText("Alpha Archived");

    await act(async () => {
      stale.resolve([activeLane]);
    });
    expect(screen.queryByText("Beta Active")).toBeNull();
    expect(screen.getByText("Alpha Archived")).toBeInTheDocument();
  });

  it("ignores loads that resolve after close and keeps the reopened view clean", async () => {
    const staleLane = lane({ title: "Stale Lane" });
    mockLists([]);
    const d = deferred<SessionSummary[]>();
    apiMocks.sessionListAll.mockReturnValueOnce(d.promise);

    const onClose = vi.fn();
    const onOpen = vi.fn();
    const onChanged = vi.fn();
    const view = (open: boolean) => (
      <SessionBrowser
        open={open}
        activeSessionId={null}
        onClose={onClose}
        onOpen={onOpen}
        onChanged={onChanged}
      />
    );
    const { rerender } = render(view(true));
    rerender(view(false));
    rerender(view(true));
    await screen.findByText("No Lanes match this view");

    await act(async () => {
      d.resolve([staleLane]);
    });
    expect(screen.queryByText("Stale Lane")).toBeNull();
    expect(screen.getByText("No Lanes match this view")).toBeInTheDocument();
  });

  it("does not blow up when a load resolves after unmount", async () => {
    mockLists([]);
    const d = deferred<SessionSummary[]>();
    apiMocks.sessionListAll.mockReturnValueOnce(d.promise);
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    const { unmount } = renderBrowser();
    unmount();
    await act(async () => {
      d.resolve([lane()]);
    });
    expect(errSpy).not.toHaveBeenCalled();
    errSpy.mockRestore();
  });
});

describe("SessionBrowser modal accessibility", () => {
  it("is a labeled modal dialog with deterministic initial focus", async () => {
    mockLists([]);
    renderBrowser();
    const dialog = screen.getByRole("dialog", { name: "Lanes" });
    expect(dialog).toHaveAttribute("aria-modal", "true");
    await screen.findByText("No Lanes match this view");
    expect(filterInput()).toHaveFocus();
  });

  it("contains Tab and Shift+Tab inside the dialog", async () => {
    mockLists([lane({ title: "Alpha" })]);
    renderBrowser();
    await screen.findByText("Alpha");

    const dialog = screen.getByRole("dialog", { name: "Lanes" });
    const focusables = Array.from(
      dialog.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), input:not([disabled])',
      ),
    );
    const first = focusables[0];
    const last = focusables[focusables.length - 1];

    last.focus();
    fireEvent.keyDown(last, { key: "Tab" });
    expect(first).toHaveFocus();

    fireEvent.keyDown(first, { key: "Tab", shiftKey: true });
    expect(last).toHaveFocus();
  });

  it("closes on Escape and restores focus to the opener", async () => {
    mockLists([]);
    function Harness() {
      const [open, setOpen] = useState(false);
      return (
        <>
          <button type="button" onClick={() => setOpen(true)}>
            Manage Lanes
          </button>
          <SessionBrowser
            open={open}
            activeSessionId={null}
            onClose={() => setOpen(false)}
            onOpen={vi.fn()}
            onChanged={vi.fn()}
          />
        </>
      );
    }
    render(<Harness />);
    const trigger = screen.getByRole("button", { name: "Manage Lanes" });
    trigger.focus();
    fireEvent.click(trigger);
    await screen.findByText("No Lanes match this view");
    expect(filterInput()).toHaveFocus();

    fireEvent.keyDown(filterInput(), { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it("lets Escape close an open dropdown without closing the browser", async () => {
    mockLists([], { folders: ["Ops"] });
    const { onClose } = renderBrowser();
    await screen.findByText("No Lanes match this view");

    fireEvent.click(screen.getByRole("button", { name: "Filter by folder" }));
    await screen.findByRole("listbox");

    fireEvent.keyDown(document.activeElement ?? document.body, {
      key: "Escape",
    });
    await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "Lanes" })).toBeInTheDocument();

    fireEvent.keyDown(document.activeElement ?? document.body, {
      key: "Escape",
    });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("focuses the filter input on the / shortcut", async () => {
    mockLists([]);
    renderBrowser();
    await screen.findByText("No Lanes match this view");

    const refresh = screen.getByRole("button", { name: "Refresh" });
    refresh.focus();
    fireEvent.keyDown(refresh, { key: "/" });
    expect(filterInput()).toHaveFocus();
  });
});

describe("SessionBrowser filters and counts", () => {
  it("filters by text with a live count, a chip, and one reset path", async () => {
    const alpha = lane({ title: "Alpha" });
    const beta = lane({ title: "Beta" });
    mockLists([alpha, beta]);
    renderBrowser();
    await screen.findByText("Alpha");
    expect(screen.getByText("2 Lanes")).toBeInTheDocument();

    fireEvent.change(filterInput(), { target: { value: "alp" } });
    expect(screen.getByText("1 of 2 Lanes shown")).toBeInTheDocument();
    expect(screen.queryByText("Beta")).toBeNull();
    expect(
      screen.getByRole("button", { name: /Clear text filter/ }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Reset filters" }));
    expect(screen.getByText("Beta")).toBeInTheDocument();
    expect(screen.getByText("2 Lanes")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Reset filters" })).toBeNull();
  });

  it("filters by tag from a row tag button and clears via the chip", async () => {
    const tagged = lane({ title: "Tagged", tags: ["ops"] });
    const plain = lane({ title: "Plain" });
    mockLists([tagged, plain], { tags: ["ops"] });
    renderBrowser();
    await screen.findByText("Tagged");

    fireEvent.click(screen.getByRole("button", { name: "Filter by tag ops" }));
    expect(screen.getByText("1 of 2 Lanes shown")).toBeInTheDocument();
    expect(screen.queryByText("Plain")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Clear tag filter" }));
    expect(screen.getByText("Plain")).toBeInTheDocument();
  });

  it("switches kind server-side, shows a chip, and reset clears it", async () => {
    const chat = lane({ title: "Chat One", kind: "chat" });
    const build = lane({ title: "Build One", kind: "build" });
    mockLists([chat, build]);
    renderBrowser();
    await screen.findByText("Chat One");

    fireEvent.click(screen.getByRole("button", { name: "Chat vs build" }));
    fireEvent.click(await screen.findByRole("option", { name: "Chats" }));
    await waitFor(() =>
      expect(apiMocks.sessionListByKind).toHaveBeenCalledWith("chat", false),
    );
    await waitFor(() => expect(screen.queryByText("Build One")).toBeNull());
    expect(
      screen.getByRole("button", { name: "Clear kind filter" }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Clear kind filter" }));
    await screen.findByText("Build One");
    expect(screen.queryByRole("button", { name: "Clear kind filter" })).toBeNull();
  });

  it("offers a reset action from the filtered-empty state", async () => {
    const only = lane({ title: "Solo" });
    mockLists([only]);
    renderBrowser();
    await screen.findByText("Solo");

    fireEvent.change(filterInput(), { target: { value: "zzz" } });
    const emptyCard = (await screen.findByText("No Lanes match this view"))
      .closest(".state-card") as HTMLElement;
    fireEvent.click(
      within(emptyCard).getByRole("button", { name: "Reset filters" }),
    );
    expect(screen.getByText("Solo")).toBeInTheDocument();
  });
});

describe("SessionBrowser selection", () => {
  it("keeps selection across text filters and discloses hidden selections", async () => {
    const alpha = lane({ title: "Alpha" });
    const beta = lane({ title: "Beta" });
    mockLists([alpha, beta]);
    renderBrowser();
    await screen.findByText("Alpha");

    fireEvent.click(screen.getByRole("checkbox", { name: "Select Lane Alpha" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Select Lane Beta" }));
    expect(screen.getByText("2 selected")).toBeInTheDocument();

    fireEvent.change(filterInput(), { target: { value: "Alpha" } });
    expect(screen.getByText("(1 hidden by filters)")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Archive selected" }));
    await waitFor(() =>
      expect(apiMocks.sessionArchive).toHaveBeenCalledTimes(2),
    );
    expect(apiMocks.sessionArchive).toHaveBeenCalledWith(alpha.id, true);
    expect(apiMocks.sessionArchive).toHaveBeenCalledWith(beta.id, true);
    await waitFor(() => expect(screen.queryByText("2 selected")).toBeNull());
  });

  it("prunes selection when a refresh drops rows", async () => {
    const alpha = lane({ title: "Alpha" });
    const beta = lane({ title: "Beta" });
    mockLists([alpha, beta]);
    renderBrowser();
    await screen.findByText("Alpha");

    fireEvent.click(screen.getByRole("checkbox", { name: "Select Lane Alpha" }));
    expect(screen.getByText("1 selected")).toBeInTheDocument();

    apiMocks.sessionListAll.mockResolvedValue([beta]);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(screen.queryByText("Alpha")).toBeNull());
    expect(screen.queryByText("1 selected")).toBeNull();
    expect(
      screen.getByRole("button", { name: "Archive selected" }),
    ).toBeDisabled();
  });

  it("clears selection when the scope changes", async () => {
    const alpha = lane({ title: "Alpha" });
    mockLists([alpha]);
    renderBrowser();
    await screen.findByText("Alpha");

    fireEvent.click(screen.getByRole("checkbox", { name: "Select Lane Alpha" }));
    expect(screen.getByText("1 selected")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "All" }));
    await waitFor(() => expect(screen.queryByText("1 selected")).toBeNull());
    await screen.findByText("Alpha");
    expect(
      screen.getByRole("checkbox", { name: "Select Lane Alpha" }),
    ).not.toBeChecked();
  });
});

describe("SessionBrowser inline editing", () => {
  it("renames with Enter, announces, and returns focus to the Rename button", async () => {
    const alpha = lane({ title: "Alpha" });
    mockLists([alpha]);
    renderBrowser();
    await screen.findByText("Alpha");

    fireEvent.click(within(rowFor("Alpha")).getByRole("button", { name: "Rename" }));
    const input = screen.getByRole("textbox", { name: "New Lane title" });
    expect(input).toHaveFocus();
    expect(input).toHaveValue("Alpha");

    fireEvent.change(input, { target: { value: "Alpha Prime" } });
    fireEvent.submit(screen.getByRole("form", { name: "Rename Lane" }));

    await waitFor(() =>
      expect(apiMocks.sessionRename).toHaveBeenCalledWith(alpha.id, "Alpha Prime"),
    );
    await waitFor(() =>
      expect(screen.queryByRole("form", { name: "Rename Lane" })).toBeNull(),
    );
    expect(screen.getByText('Lane renamed to "Alpha Prime"')).toBeInTheDocument();
    await waitFor(() =>
      expect(
        within(rowFor("Alpha")).getByRole("button", { name: "Rename" }),
      ).toHaveFocus(),
    );
  });

  it("refuses to save an empty title", async () => {
    const alpha = lane({ title: "Alpha" });
    mockLists([alpha]);
    renderBrowser();
    await screen.findByText("Alpha");

    fireEvent.click(within(rowFor("Alpha")).getByRole("button", { name: "Rename" }));
    const input = screen.getByRole("textbox", { name: "New Lane title" });
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.submit(screen.getByRole("form", { name: "Rename Lane" }));

    expect(apiMocks.sessionRename).not.toHaveBeenCalled();
    expect(screen.getByRole("form", { name: "Rename Lane" })).toBeInTheDocument();
  });

  it("cancels an editor with Escape before Escape closes the browser", async () => {
    const alpha = lane({ title: "Alpha" });
    mockLists([alpha]);
    const { onClose } = renderBrowser();
    await screen.findByText("Alpha");

    fireEvent.click(within(rowFor("Alpha")).getByRole("button", { name: "Rename" }));
    const input = screen.getByRole("textbox", { name: "New Lane title" });
    fireEvent.keyDown(input, { key: "Escape" });

    expect(screen.queryByRole("form", { name: "Rename Lane" })).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    const renameBtn = within(rowFor("Alpha")).getByRole("button", {
      name: "Rename",
    });
    expect(renameBtn).toHaveFocus();

    fireEvent.keyDown(renameBtn, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("saves an empty folder as null (back to Inbox)", async () => {
    const gamma = lane({ title: "Gamma", folder: "Ops" });
    mockLists([gamma], { folders: ["Ops"] });
    renderBrowser();
    await screen.findByText("Gamma");

    fireEvent.click(within(rowFor("Gamma")).getByRole("button", { name: "Folder" }));
    // The folder input carries a datalist, which maps to the combobox role.
    const input = screen.getByRole("combobox", { name: "Folder name" });
    expect(input).toHaveValue("Ops");
    fireEvent.change(input, { target: { value: "" } });
    fireEvent.submit(screen.getByRole("form", { name: "Set Lane folder" }));

    await waitFor(() =>
      expect(apiMocks.sessionSetFolder).toHaveBeenCalledWith(gamma.id, null),
    );
    await screen.findByText("Lane moved to Inbox");
  });

  it("normalizes tag input before saving", async () => {
    const alpha = lane({ title: "Alpha", tags: ["old"] });
    mockLists([alpha], { tags: ["old"] });
    renderBrowser();
    await screen.findByText("Alpha");

    fireEvent.click(within(rowFor("Alpha")).getByRole("button", { name: "Tags" }));
    const input = screen.getByRole("textbox", { name: "Tags" });
    expect(input).toHaveValue("old");
    fireEvent.change(input, { target: { value: "ops, #infra  extra" } });
    fireEvent.submit(screen.getByRole("form", { name: "Set Lane tags" }));

    await waitFor(() =>
      expect(apiMocks.sessionSetTags).toHaveBeenCalledWith(alpha.id, [
        "ops",
        "infra",
        "extra",
      ]),
    );
  });
});

describe("SessionBrowser destructive actions", () => {
  it("asks a specific confirmation naming the Lane, and cancel restores focus", async () => {
    const alpha = lane({ title: "Alpha" });
    mockLists([alpha]);
    renderBrowser();
    await screen.findByText("Alpha");

    fireEvent.click(within(rowFor("Alpha")).getByRole("button", { name: "Delete" }));
    const confirm = screen.getByRole("alertdialog", { name: "Delete this Lane?" });
    expect(within(confirm).getByText(/“Alpha” is permanently deleted/)).toBeInTheDocument();
    expect(within(confirm).getByRole("button", { name: "Cancel" })).toHaveFocus();

    fireEvent.click(within(confirm).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(apiMocks.sessionDelete).not.toHaveBeenCalled();
    expect(
      within(rowFor("Alpha")).getByRole("button", { name: "Delete" }),
    ).toHaveFocus();
  });

  it("closes only the confirmation on Escape", async () => {
    const alpha = lane({ title: "Alpha" });
    mockLists([alpha]);
    const { onClose } = renderBrowser();
    await screen.findByText("Alpha");

    fireEvent.click(within(rowFor("Alpha")).getByRole("button", { name: "Delete" }));
    const confirm = screen.getByRole("alertdialog", { name: "Delete this Lane?" });
    fireEvent.keyDown(within(confirm).getByRole("button", { name: "Cancel" }), {
      key: "Escape",
    });
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(screen.getByRole("dialog", { name: "Lanes" })).toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
    expect(apiMocks.sessionDelete).not.toHaveBeenCalled();
  });

  it("bulk-deletes sequentially with visible progress and an announcement", async () => {
    const alpha = lane({ title: "Alpha" });
    const beta = lane({ title: "Beta" });
    mockLists([alpha, beta]);
    renderBrowser();
    await screen.findByText("Alpha");

    fireEvent.click(screen.getByRole("checkbox", { name: "Select Lane Alpha" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Select Lane Beta" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete selected" }));

    const confirm = screen.getByRole("alertdialog", { name: "Delete 2 Lanes?" });
    expect(within(confirm).getByText("Alpha")).toBeInTheDocument();
    expect(within(confirm).getByText("Beta")).toBeInTheDocument();

    const d1 = deferred<unknown>();
    const d2 = deferred<unknown>();
    apiMocks.sessionDelete
      .mockReturnValueOnce(d1.promise)
      .mockReturnValueOnce(d2.promise);

    fireEvent.click(within(confirm).getByRole("button", { name: "Delete 2 Lanes" }));
    expect(await screen.findByText("Deleting 1 of 2…")).toBeInTheDocument();
    expect(apiMocks.sessionDelete).toHaveBeenCalledTimes(1);

    await act(async () => {
      d1.resolve(undefined);
    });
    expect(screen.getByText("Deleting 2 of 2…")).toBeInTheDocument();
    expect(apiMocks.sessionDelete).toHaveBeenCalledTimes(2);

    await act(async () => {
      d2.resolve(undefined);
    });
    await screen.findByText("Deleted 2 Lanes");
    expect(apiMocks.sessionDelete).toHaveBeenCalledWith(alpha.id);
    expect(apiMocks.sessionDelete).toHaveBeenCalledWith(beta.id);
    expect(screen.queryByText(/Deleting/)).toBeNull();
  });

  it("fences double submits while an operation is in flight", async () => {
    const alpha = lane({ title: "Alpha" });
    const beta = lane({ title: "Beta" });
    mockLists([alpha, beta]);
    renderBrowser();
    await screen.findByText("Alpha");

    fireEvent.click(screen.getByRole("checkbox", { name: "Select Lane Alpha" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Select Lane Beta" }));

    const d1 = deferred<unknown>();
    apiMocks.sessionArchive
      .mockReturnValueOnce(d1.promise)
      .mockResolvedValue(undefined);

    const archiveBtn = screen.getByRole("button", { name: "Archive selected" });
    fireEvent.click(archiveBtn);
    fireEvent.click(archiveBtn);
    fireEvent.click(archiveBtn);
    expect(apiMocks.sessionArchive).toHaveBeenCalledTimes(1);

    await act(async () => {
      d1.resolve(undefined);
    });
    await waitFor(() =>
      expect(apiMocks.sessionArchive).toHaveBeenCalledTimes(2),
    );
    await screen.findByText("Archived 2 Lanes");
    expect(apiMocks.sessionArchive).toHaveBeenCalledTimes(2);
  });

  it("keeps the list usable after an action error, with retry", async () => {
    const alpha = lane({ title: "Alpha" });
    mockLists([alpha]);
    renderBrowser();
    await screen.findByText("Alpha");

    apiMocks.sessionDelete.mockRejectedValueOnce("disk locked");
    fireEvent.click(within(rowFor("Alpha")).getByRole("button", { name: "Delete" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete Lane" }));

    await screen.findByText("Delete failed");
    expect(screen.getByText("disk locked")).toBeInTheDocument();
    expect(screen.getByText("Alpha")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByText("Deleted 1 Lane");
    expect(apiMocks.sessionDelete).toHaveBeenCalledTimes(2);
    expect(apiMocks.sessionDelete).toHaveBeenLastCalledWith(alpha.id);
    expect(screen.queryByText("Delete failed")).toBeNull();
  });

  it("lets the user dismiss an action error", async () => {
    const alpha = lane({ title: "Alpha" });
    mockLists([alpha]);
    renderBrowser();
    await screen.findByText("Alpha");

    apiMocks.sessionArchive.mockRejectedValueOnce("nope");
    fireEvent.click(within(rowFor("Alpha")).getByRole("button", { name: "Archive" }));

    await screen.findByText("Archive failed");
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByText("Archive failed")).toBeNull();
  });
});

describe("SessionBrowser archived Lane boundary", () => {
  it("inspects without restoring and provides a separate Restore action", async () => {
    const archived = lane({
      id: "lane-archived",
      title: "Previous gateway experiment",
      archived: true,
      message_count: 12,
    });
    apiMocks.sessionListAll.mockResolvedValue([]);
    apiMocks.sessionListArchived.mockResolvedValue([archived]);
    apiMocks.sessionListFolders.mockResolvedValue([]);
    apiMocks.sessionListTags.mockResolvedValue([]);
    apiMocks.sessionArchive.mockResolvedValue({ ...archived, archived: false });
    const { onOpen, onChanged } = renderBrowser();

    await screen.findByText("No Lanes match this view");
    fireEvent.click(screen.getByRole("button", { name: "Archive" }));
    const inspect = await screen.findByRole("button", { name: "Inspect" });
    fireEvent.click(inspect);
    expect(onOpen).toHaveBeenCalledWith(archived);
    expect(apiMocks.sessionArchive).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Restore" }));
    await waitFor(() => {
      expect(apiMocks.sessionArchive).toHaveBeenCalledWith(archived.id, false);
    });
    expect(onChanged).toHaveBeenCalled();
  });
});
