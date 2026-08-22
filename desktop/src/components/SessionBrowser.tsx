import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { api } from "../lib/api";
import type { SessionSummary } from "../lib/protocol";
import { StateCard } from "./StateCard";
import { StyledSelect } from "./StyledSelect";
import "./SessionBrowser.css";

export type SessionBrowserProps = {
  open: boolean;
  activeSessionId: string | null;
  onClose: () => void;
  onOpen: (s: SessionSummary) => void;
  onChanged: () => void;
};

type Scope = "active" | "archive" | "all";
type KindFilter = "all" | "chat" | "build";
type EditorMode = "rename" | "folder" | "tags";
type Editor = { mode: EditorMode; id: string; draft: string };
type ConfirmDelete = { ids: string[]; titles: string[] };
type Progress = { verb: string; done: number; total: number };
type ActionError = { label: string; message: string; retry: () => void };

const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

function fmtDate(iso: string): string {
  try {
    return new Date(iso).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return iso;
  }
}

function normalizeTags(raw: string): string[] {
  return raw
    .split(/[,#\s]+/)
    .map((t) => t.trim())
    .filter(Boolean);
}

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

/**
 * Full-screen Lane manager: rename, delete, archive, folders, tags.
 */
export function SessionBrowser({
  open,
  activeSessionId,
  onClose,
  onOpen,
  onChanged,
}: SessionBrowserProps) {
  const [scope, setScope] = useState<Scope>("active");
  const [kindFilter, setKindFilter] = useState<KindFilter>("all");
  const [rows, setRows] = useState<SessionSummary[]>([]);
  const [folders, setFolders] = useState<string[]>([]);
  const [allTags, setAllTags] = useState<string[]>([]);
  const [query, setQuery] = useState("");
  const [folderFilter, setFolderFilter] = useState<string | "">("");
  const [tagFilter, setTagFilter] = useState<string | "">("");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<ActionError | null>(null);
  const [editor, setEditor] = useState<Editor | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<ConfirmDelete | null>(null);
  const [progress, setProgress] = useState<Progress | null>(null);
  const [announce, setAnnounce] = useState("");

  // A refresh only applies while its generation is current; scope/kind changes,
  // close, and unmount all invalidate in-flight loads.
  const genRef = useRef(0);
  const busyRef = useRef(false);
  const completedRef = useRef(0);
  const rowsRef = useRef<SessionSummary[]>([]);
  const dialogRef = useRef<HTMLDivElement>(null);
  const confirmRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);
  const pendingFocusRef = useRef<string | null>(null);
  const confirmReturnRef = useRef<string | null>(null);

  const refresh = useCallback(async () => {
    const gen = ++genRef.current;
    setLoading(true);
    setLoadError(null);
    try {
      const includeArchived = scope !== "active";
      let list: SessionSummary[];
      if (kindFilter === "all") {
        list =
          scope === "active"
            ? await api.sessionListAll().then((all) =>
                all.filter((s) => !s.archived),
              )
            : scope === "archive"
              ? await api.sessionListArchived()
              : await api.sessionListAll();
      } else {
        list = await api.sessionListByKind(
          kindFilter,
          scope === "archive" || scope === "all",
        );
        if (scope === "active") list = list.filter((s) => !s.archived);
        if (scope === "archive") list = list.filter((s) => s.archived);
      }
      const [f, t] = await Promise.all([
        api.sessionListFolders(includeArchived),
        api.sessionListTags(includeArchived),
      ]);
      if (gen !== genRef.current) return;
      setRows(list);
      setFolders(f);
      setAllTags(t);
    } catch (e) {
      if (gen !== genRef.current) return;
      setLoadError(String(e));
    } finally {
      if (gen === genRef.current) setLoading(false);
    }
  }, [scope, kindFilter]);

  const refreshRef = useRef(refresh);
  useEffect(() => {
    refreshRef.current = refresh;
  }, [refresh]);

  useEffect(() => {
    if (!open) return;
    void refresh();
  }, [open, refresh]);

  // Closing or unmounting invalidates in-flight loads and transient overlays.
  useEffect(() => {
    if (open) return;
    genRef.current += 1;
    setEditor(null);
    setConfirmDelete(null);
    setProgress(null);
    setActionError(null);
    setAnnounce("");
  }, [open]);
  useEffect(
    () => () => {
      genRef.current += 1;
    },
    [],
  );

  useEffect(() => {
    rowsRef.current = rows;
    // Selection only ever refers to Lanes that still exist in the loaded scope.
    setSelected((prev) => {
      if (prev.size === 0) return prev;
      const valid = new Set(rows.map((r) => r.id));
      const next = new Set([...prev].filter((id) => valid.has(id)));
      return next.size === prev.size ? prev : next;
    });
  }, [rows]);

  // Deterministic initial focus on open; restore the opener's focus on close.
  useEffect(() => {
    if (open) {
      restoreFocusRef.current = document.activeElement as HTMLElement | null;
      searchRef.current?.focus();
    } else if (restoreFocusRef.current) {
      restoreFocusRef.current.focus?.();
      restoreFocusRef.current = null;
    }
  }, [open]);
  useEffect(
    () => () => {
      restoreFocusRef.current?.focus?.();
    },
    [],
  );

  // After an editor or the confirm dialog closes, hand focus back to the
  // control that opened it ("search" = the always-present filter input).
  // Deferred while busy so the target is enabled again when focused.
  useEffect(() => {
    if (editor || confirmDelete || busy) return;
    const key = pendingFocusRef.current;
    if (!key) return;
    pendingFocusRef.current = null;
    if (key === "search") {
      searchRef.current?.focus();
      return;
    }
    const el = dialogRef.current?.querySelector<HTMLElement>(
      `[data-focus-key="${key}"]`,
    );
    if (el) el.focus();
    else searchRef.current?.focus();
  }, [editor, confirmDelete, busy]);

  const cancelEditor = useCallback(() => {
    setEditor((current) => {
      if (current) pendingFocusRef.current = `${current.mode}:${current.id}`;
      return null;
    });
  }, []);

  const cancelConfirm = useCallback(() => {
    pendingFocusRef.current = confirmReturnRef.current ?? "search";
    confirmReturnRef.current = null;
    setConfirmDelete(null);
  }, []);

  // Escape resolves innermost-first: dropdown menu, confirm dialog, inline
  // editor, then the browser itself. "/" jumps to the filter input.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (
        e.key === "/" &&
        !confirmDelete &&
        !e.defaultPrevented &&
        !isEditableTarget(e.target)
      ) {
        e.preventDefault();
        searchRef.current?.focus();
        return;
      }
      if (e.key !== "Escape" || e.defaultPrevented) return;
      if (document.querySelector(".styled-select-menu")) return;
      if (confirmDelete) {
        e.preventDefault();
        cancelConfirm();
        return;
      }
      if (editor) {
        e.preventDefault();
        cancelEditor();
        return;
      }
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, editor, confirmDelete, onClose, cancelConfirm, cancelEditor]);

  // Tab containment; scoped to the confirm dialog while it is up.
  function handleTrapKeyDown(e: ReactKeyboardEvent<HTMLDivElement>) {
    if (e.key !== "Tab") return;
    const root = confirmDelete ? confirmRef.current : dialogRef.current;
    if (!root) return;
    const nodes = Array.from(
      root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
    ).filter((el) => el.getAttribute("aria-hidden") !== "true");
    if (!nodes.length) return;
    const first = nodes[0];
    const last = nodes[nodes.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (confirmDelete && (!active || !root.contains(active))) {
      e.preventDefault();
      first.focus();
      return;
    }
    if (e.shiftKey && active === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    }
  }

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return rows.filter((s) => {
      if (folderFilter === "__inbox__") {
        if (s.folder) return false;
      } else if (folderFilter) {
        if ((s.folder ?? "") !== folderFilter) return false;
      }
      if (tagFilter) {
        if (!(s.tags ?? []).includes(tagFilter)) return false;
      }
      if (!q) return true;
      const hay = [s.title, s.cwd, s.folder ?? "", ...(s.tags ?? [])]
        .join(" ")
        .toLowerCase();
      return hay.includes(q);
    });
  }, [rows, query, folderFilter, tagFilter]);

  const byFolder = useMemo(() => {
    const map = new Map<string, SessionSummary[]>();
    for (const s of filtered) {
      const key = s.folder?.trim() || "Inbox";
      const list = map.get(key) ?? [];
      list.push(s);
      map.set(key, list);
    }
    // Stable order: Inbox first, then alpha
    const keys = [...map.keys()].sort((a, b) => {
      if (a === "Inbox") return -1;
      if (b === "Inbox") return 1;
      return a.localeCompare(b);
    });
    return keys.map((k) => ({ folder: k, items: map.get(k)! }));
  }, [filtered]);

  const visibleSelectedCount = useMemo(
    () => filtered.reduce((n, s) => n + (selected.has(s.id) ? 1 : 0), 0),
    [filtered, selected],
  );
  const hiddenSelectedCount = selected.size - visibleSelectedCount;
  const clientFilterActive =
    query.trim() !== "" || folderFilter !== "" || tagFilter !== "";
  const anyFilterActive = clientFilterActive || kindFilter !== "all";

  function toggleSelect(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function selectAllVisible() {
    setSelected(new Set(filtered.map((s) => s.id)));
  }

  function clearKindFilter() {
    setKindFilter("all");
    setSelected(new Set());
  }

  function resetFilters() {
    setQuery("");
    setFolderFilter("");
    setTagFilter("");
    if (kindFilter !== "all") clearKindFilter();
  }

  // Single gate for every mutation: synchronous double-submit fence, action
  // errors that can be retried, and a refresh even after partial failures so
  // the list reflects what actually happened.
  async function runOp(label: string, op: () => Promise<string>) {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setActionError(null);
    setAnnounce("");
    completedRef.current = 0;
    let succeeded = false;
    try {
      const message = await op();
      succeeded = true;
      setAnnounce(message);
    } catch (e) {
      setActionError({
        label,
        message: String(e),
        retry: () => {
          void runOp(label, op);
        },
      });
    } finally {
      setProgress(null);
      busyRef.current = false;
      setBusy(false);
      if (succeeded || completedRef.current > 0) onChanged();
      void refreshRef.current();
    }
  }

  async function eachWithProgress(
    ids: string[],
    verb: string,
    fn: (id: string) => Promise<unknown>,
  ) {
    const total = ids.length;
    if (total > 1) setProgress({ verb, done: 0, total });
    for (let index = 0; index < total; index += 1) {
      await fn(ids[index]);
      completedRef.current = index + 1;
      if (total > 1) setProgress({ verb, done: index + 1, total });
    }
  }

  function startEditor(mode: EditorMode, s: SessionSummary) {
    if (busy) return;
    const draft =
      mode === "rename"
        ? s.title
        : mode === "folder"
          ? (s.folder ?? "")
          : (s.tags ?? []).join(", ");
    setEditor({ mode, id: s.id, draft });
  }

  function finishEditor(mode: EditorMode, id: string) {
    pendingFocusRef.current = `${mode}:${id}`;
    setEditor(null);
  }

  function saveEditor() {
    if (!editor) return;
    const { mode, id, draft } = editor;
    if (mode === "rename") {
      const title = draft.trim();
      if (!title) return;
      void runOp("Rename", async () => {
        await api.sessionRename(id, title);
        finishEditor(mode, id);
        return `Lane renamed to "${title}"`;
      });
    } else if (mode === "folder") {
      const folder = draft.trim() || null;
      void runOp("Move to folder", async () => {
        await api.sessionSetFolder(id, folder);
        finishEditor(mode, id);
        return folder ? `Lane moved to folder ${folder}` : "Lane moved to Inbox";
      });
    } else {
      const tags = normalizeTags(draft);
      void runOp("Update tags", async () => {
        await api.sessionSetTags(id, tags);
        finishEditor(mode, id);
        return "Lane tags updated";
      });
    }
  }

  function archiveLanes(ids: string[], archived: boolean, fromBulk: boolean) {
    if (!ids.length) return;
    const label = archived ? "Archive" : "Restore";
    void runOp(label, async () => {
      await eachWithProgress(
        ids,
        archived ? "Archiving" : "Restoring",
        (id) => api.sessionArchive(id, archived),
      );
      if (fromBulk) setSelected(new Set());
      const noun = ids.length === 1 ? "Lane" : `${ids.length} Lanes`;
      return `${archived ? "Archived" : "Restored"} ${noun}`;
    });
  }

  function requestDelete(ids: string[], returnKey: string) {
    const targets = rows.filter((r) => ids.includes(r.id));
    if (!targets.length) return;
    confirmReturnRef.current = returnKey;
    setConfirmDelete({
      ids: targets.map((t) => t.id),
      titles: targets.map((t) => t.title),
    });
  }

  function executeDelete() {
    if (!confirmDelete || busyRef.current) return;
    const ids = [...confirmDelete.ids];
    pendingFocusRef.current = "search";
    confirmReturnRef.current = null;
    setConfirmDelete(null);
    void runOp("Delete", async () => {
      // Retry after a partial failure only touches Lanes that still exist.
      const targets = ids.filter((id) =>
        rowsRef.current.some((r) => r.id === id),
      );
      await eachWithProgress(targets, "Deleting", (id) =>
        api.sessionDelete(id),
      );
      setSelected((prev) => {
        const next = new Set(prev);
        for (const id of targets) next.delete(id);
        return next;
      });
      return targets.length === 1 ? "Deleted 1 Lane" : `Deleted ${targets.length} Lanes`;
    });
  }

  function bulkFolder(folder: string | null) {
    const ids = [...selected];
    if (!ids.length) return;
    void runOp("Move to folder", async () => {
      await eachWithProgress(ids, "Moving", (id) =>
        api.sessionSetFolder(id, folder),
      );
      const noun = ids.length === 1 ? "Lane" : `${ids.length} Lanes`;
      return `Moved ${noun} to ${folder ?? "Inbox"}`;
    });
  }

  if (!open) return null;

  const shown = filtered.length;
  const countText = loading
    ? "Loading Lanes…"
    : clientFilterActive
      ? `${shown} of ${rows.length} Lanes shown`
      : `${shown} Lane${shown === 1 ? "" : "s"}`;

  const renderEditorForm = (
    s: SessionSummary,
    mode: EditorMode,
    formLabel: string,
    inputLabel: string,
    saveLabel: string,
    placeholder?: string,
  ) =>
    editor?.mode === mode && editor.id === s.id ? (
      <form
        className="sb-inline-form"
        aria-label={formLabel}
        onSubmit={(e) => {
          e.preventDefault();
          saveEditor();
        }}
      >
        <input
          autoFocus
          aria-label={inputLabel}
          placeholder={placeholder}
          list={mode === "folder" ? "sb-folder-suggestions" : undefined}
          value={editor.draft}
          onChange={(e) =>
            setEditor((cur) => (cur ? { ...cur, draft: e.target.value } : cur))
          }
        />
        {mode === "folder" && (
          <datalist id="sb-folder-suggestions">
            {folders.map((f) => (
              <option key={f} value={f} />
            ))}
          </datalist>
        )}
        <button type="submit" className="primary" disabled={busy}>
          {saveLabel}
        </button>
        <button type="button" onClick={cancelEditor}>
          Cancel
        </button>
      </form>
    ) : null;

  return (
    <div
      className="session-browser"
      role="dialog"
      aria-modal="true"
      aria-labelledby="sb-dialog-title"
      ref={dialogRef}
      onKeyDown={handleTrapKeyDown}
    >
      <header className="sb-header">
        <div className="sb-title-block">
          <h2 id="sb-dialog-title">Lanes</h2>
          <span className="sb-sub">
            Organize active work and inspect archived work
          </span>
        </div>
        <div className="sb-header-actions">
          <button
            type="button"
            className="primary"
            disabled={busy || loading}
            onClick={() => void refresh()}
          >
            Refresh
          </button>
          <button
            type="button"
            aria-label="Close Lanes"
            aria-keyshortcuts="Escape"
            onClick={onClose}
          >
            Close
            <kbd className="sb-kbd" aria-hidden>
              Esc
            </kbd>
          </button>
        </div>
      </header>

      <div className="sb-toolbar">
        <div className="sb-scopes" role="group" aria-label="Lane archive scope">
          {(
            [
              ["active", "Active"],
              ["archive", "Archive"],
              ["all", "All"],
            ] as const
          ).map(([id, label]) => (
            <button
              key={id}
              type="button"
              aria-pressed={scope === id}
              className={scope === id ? "active" : ""}
              onClick={() => {
                setScope(id);
                setSelected(new Set());
              }}
            >
              {label}
            </button>
          ))}
        </div>
        <StyledSelect
          aria-label="Chat vs build"
          className="sb-select"
          value={kindFilter}
          options={[
            { value: "all", label: "All kinds" },
            { value: "chat", label: "Chats" },
            { value: "build", label: "Builds" },
          ]}
          onChange={(v) => {
            setKindFilter(v as KindFilter);
            setSelected(new Set());
          }}
        />
        <input
          ref={searchRef}
          className="sb-search"
          aria-label="Filter Lanes"
          aria-keyshortcuts="/"
          placeholder="Filter by title, path, folder, tag…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <StyledSelect
          aria-label="Filter by folder"
          className="sb-select"
          value={folderFilter}
          options={[
            { value: "", label: "All folders" },
            { value: "__inbox__", label: "Inbox (no folder)" },
            ...folders.map((f) => ({ value: f, label: f })),
          ]}
          onChange={(v) => setFolderFilter(v)}
        />
        <StyledSelect
          aria-label="Filter by tag"
          className="sb-select"
          value={tagFilter}
          options={[
            { value: "", label: "All tags" },
            ...allTags.map((t) => ({ value: t, label: `#${t}` })),
          ]}
          onChange={(v) => setTagFilter(v)}
        />
      </div>

      <div className="sb-summary">
        <span className="sb-count" role="status">
          {countText}
        </span>
        {query.trim() && (
          <button
            type="button"
            className="sb-chip"
            aria-label={`Clear text filter “${query.trim()}”`}
            onClick={() => setQuery("")}
          >
            <span className="sb-chip-label">“{query.trim()}”</span>
            <span className="sb-chip-x" aria-hidden>
              ×
            </span>
          </button>
        )}
        {folderFilter && (
          <button
            type="button"
            className="sb-chip"
            aria-label="Clear folder filter"
            onClick={() => setFolderFilter("")}
          >
            <span className="sb-chip-label">
              Folder: {folderFilter === "__inbox__" ? "Inbox" : folderFilter}
            </span>
            <span className="sb-chip-x" aria-hidden>
              ×
            </span>
          </button>
        )}
        {tagFilter && (
          <button
            type="button"
            className="sb-chip"
            aria-label="Clear tag filter"
            onClick={() => setTagFilter("")}
          >
            <span className="sb-chip-label">#{tagFilter}</span>
            <span className="sb-chip-x" aria-hidden>
              ×
            </span>
          </button>
        )}
        {kindFilter !== "all" && (
          <button
            type="button"
            className="sb-chip"
            aria-label="Clear kind filter"
            onClick={clearKindFilter}
          >
            <span className="sb-chip-label">
              {kindFilter === "chat" ? "Chats" : "Builds"}
            </span>
            <span className="sb-chip-x" aria-hidden>
              ×
            </span>
          </button>
        )}
        {anyFilterActive && (
          <button type="button" className="sb-reset" onClick={resetFilters}>
            Reset filters
          </button>
        )}
      </div>

      <div className="sb-bulk">
        <button
          type="button"
          disabled={!filtered.length}
          onClick={selectAllVisible}
        >
          Select visible ({filtered.length})
        </button>
        <button
          type="button"
          disabled={!selected.size}
          onClick={() => setSelected(new Set())}
        >
          Clear selection
        </button>
        <button
          type="button"
          disabled={!selected.size || busy}
          onClick={() => archiveLanes([...selected], scope !== "archive", true)}
        >
          {scope === "archive" ? "Restore selected" : "Archive selected"}
        </button>
        <button
          type="button"
          className="danger"
          data-focus-key="bulk-delete"
          disabled={!selected.size || busy}
          onClick={() => requestDelete([...selected], "bulk-delete")}
        >
          Delete selected
        </button>
        <StyledSelect
          aria-label="Move selected to folder"
          className="sb-select"
          disabled={!selected.size || busy}
          value=""
          options={[
            { value: "", label: "Move selected to folder…" },
            { value: "__clear__", label: "Inbox (clear folder)" },
            ...folders.map((f) => ({ value: f, label: f })),
          ]}
          onChange={(v) => {
            if (!v) return;
            bulkFolder(v === "__clear__" ? null : v);
          }}
        />
        {selected.size > 0 && (
          <span className="sb-sel-count">
            {selected.size} selected
            {hiddenSelectedCount > 0 && (
              <span className="sb-sel-hidden">
                {" "}
                ({hiddenSelectedCount} hidden by filters)
              </span>
            )}
          </span>
        )}
      </div>

      {progress && (
        <div className="sb-progress" role="status">
          <span>
            {progress.verb} {Math.min(progress.done + 1, progress.total)} of{" "}
            {progress.total}…
          </span>
          <span className="sb-progress-track" aria-hidden>
            <span
              className="sb-progress-fill"
              style={{
                width: `${Math.round((progress.done / progress.total) * 100)}%`,
                display: "block",
              }}
            />
          </span>
        </div>
      )}

      <div className="sr-only" role="status" aria-live="polite">
        {announce}
      </div>

      {loadError && (
        <StateCard
          variant="error"
          title="Lanes could not be loaded"
          description={loadError}
          actionLabel="Try again"
          onAction={() => void refresh()}
        />
      )}

      {actionError && (
        <StateCard
          variant="error"
          title={`${actionError.label} failed`}
          description={actionError.message}
          actionLabel="Try again"
          onAction={actionError.retry}
        >
          <button type="button" onClick={() => setActionError(null)}>
            Dismiss
          </button>
        </StateCard>
      )}

      <div className="sb-body" aria-busy={loading}>
        <div className="sb-body-inner">
          {loading && rows.length === 0 && (
            <StateCard
              variant="loading"
              title="Loading Lanes"
              description="Reading active and archived work…"
            />
          )}
          {!loading && !loadError && byFolder.length === 0 && (
            <StateCard
              variant="empty"
              title="No Lanes match this view"
              description="Change the filters or choose another archive scope."
              actionLabel={anyFilterActive ? "Reset filters" : undefined}
              onAction={anyFilterActive ? resetFilters : undefined}
            />
          )}
          {!loadError &&
            byFolder.map(({ folder, items }) => (
              <section key={folder} className="sb-folder-group">
                <h3 className="sb-folder-heading">
                  <span className="sb-folder-icon" aria-hidden>
                    {folder === "Inbox" ? "◎" : "▣"}
                  </span>
                  {folder}
                  <span className="sb-folder-count">{items.length}</span>
                </h3>
                <ul className="sb-list">
                  {items.map((s) => {
                    const isActive = s.id === activeSessionId;
                    const isSelected = selected.has(s.id);
                    return (
                      <li
                        key={s.id}
                        className={`sb-row ${isActive ? "active" : ""} ${isSelected ? "selected" : ""} ${s.archived ? "archived" : ""}`}
                      >
                        <label className="sb-check">
                          <input
                            type="checkbox"
                            aria-label={`Select Lane ${s.title}`}
                            checked={isSelected}
                            onChange={() => toggleSelect(s.id)}
                          />
                        </label>
                        <div className="sb-main">
                          {editor?.mode === "rename" && editor.id === s.id ? (
                            renderEditorForm(
                              s,
                              "rename",
                              "Rename Lane",
                              "New Lane title",
                              "Save",
                            )
                          ) : (
                            <button
                              type="button"
                              className="sb-title-btn"
                              onClick={() => onOpen(s)}
                              aria-label={`${s.archived ? "Inspect archived" : "Open"} Lane ${s.title}`}
                              title={
                                s.archived ? "Inspect archived Lane" : "Open Lane"
                              }
                            >
                              <span className={`sp-kind ${s.kind ?? "build"}`}>
                                {s.kind ?? "build"}
                              </span>
                              {s.title}
                              {isActive && <span className="sb-pill">open</span>}
                              {s.archived && (
                                <span className="sb-pill archive">archived</span>
                              )}
                            </button>
                          )}
                          <div className="sb-meta">
                            <span>{s.message_count} msgs</span>
                            <span>·</span>
                            <span title={s.updated_at}>
                              {fmtDate(s.updated_at)}
                            </span>
                            {s.cwd && (
                              <>
                                <span>·</span>
                                <span className="sb-cwd" title={s.cwd}>
                                  {s.cwd.split("/").slice(-2).join("/")}
                                </span>
                              </>
                            )}
                          </div>
                          {(s.tags?.length ?? 0) > 0 && (
                            <div className="sb-tags">
                              {s.tags!.map((t) => (
                                <button
                                  key={t}
                                  type="button"
                                  className="sb-tag"
                                  aria-label={`Filter by tag ${t}`}
                                  onClick={() => setTagFilter(t)}
                                >
                                  #{t}
                                </button>
                              ))}
                            </div>
                          )}
                          {renderEditorForm(
                            s,
                            "folder",
                            "Set Lane folder",
                            "Folder name",
                            "Save folder",
                            "Folder name (empty = Inbox)",
                          )}
                          {renderEditorForm(
                            s,
                            "tags",
                            "Set Lane tags",
                            "Tags",
                            "Save tags",
                            "tags, comma separated",
                          )}
                        </div>
                        <div className="sb-actions">
                          <button
                            type="button"
                            data-focus-key={`rename:${s.id}`}
                            disabled={busy}
                            onClick={() => startEditor("rename", s)}
                          >
                            Rename
                          </button>
                          <button
                            type="button"
                            data-focus-key={`folder:${s.id}`}
                            disabled={busy}
                            onClick={() => startEditor("folder", s)}
                          >
                            Folder
                          </button>
                          <button
                            type="button"
                            data-focus-key={`tags:${s.id}`}
                            disabled={busy}
                            onClick={() => startEditor("tags", s)}
                          >
                            Tags
                          </button>
                          <button
                            type="button"
                            disabled={busy}
                            onClick={() => archiveLanes([s.id], !s.archived, false)}
                          >
                            {s.archived ? "Restore" : "Archive"}
                          </button>
                          <button
                            type="button"
                            className="danger"
                            data-focus-key={`delete:${s.id}`}
                            disabled={busy}
                            onClick={() => requestDelete([s.id], `delete:${s.id}`)}
                          >
                            Delete
                          </button>
                          <button
                            type="button"
                            className="primary"
                            onClick={() => onOpen(s)}
                          >
                            {s.archived ? "Inspect" : "Open"}
                          </button>
                        </div>
                      </li>
                    );
                  })}
                </ul>
              </section>
            ))}
        </div>
      </div>

      {confirmDelete && (
        <div className="sb-confirm-overlay">
          <div
            className="sb-confirm"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="sb-confirm-title"
            aria-describedby="sb-confirm-desc"
            ref={confirmRef}
          >
            <h3 id="sb-confirm-title">
              {confirmDelete.ids.length === 1
                ? "Delete this Lane?"
                : `Delete ${confirmDelete.ids.length} Lanes?`}
            </h3>
            <p id="sb-confirm-desc">
              {confirmDelete.ids.length === 1
                ? `“${confirmDelete.titles[0]}” is permanently deleted and its transcript is removed from disk.`
                : "These Lanes are permanently deleted and their transcripts are removed from disk."}
            </p>
            {confirmDelete.ids.length > 1 && (
              <ul className="sb-confirm-list">
                {confirmDelete.titles.slice(0, 8).map((t, i) => (
                  <li key={`${confirmDelete.ids[i]}`}>{t}</li>
                ))}
                {confirmDelete.titles.length > 8 && (
                  <li>…and {confirmDelete.titles.length - 8} more</li>
                )}
              </ul>
            )}
            <div className="sb-confirm-actions">
              <button type="button" autoFocus onClick={cancelConfirm}>
                Cancel
              </button>
              <button
                type="button"
                className="danger"
                disabled={busy}
                onClick={executeDelete}
              >
                {confirmDelete.ids.length === 1
                  ? "Delete Lane"
                  : `Delete ${confirmDelete.ids.length} Lanes`}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
