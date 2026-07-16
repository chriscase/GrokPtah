import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import type { SessionSummary } from "../lib/protocol";

export type SessionBrowserProps = {
  open: boolean;
  activeSessionId: string | null;
  onClose: () => void;
  onOpen: (s: SessionSummary) => void;
  onChanged: () => void;
};

type Scope = "active" | "archive" | "all";
type KindFilter = "all" | "chat" | "build";

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

/**
 * Full-screen session manager: rename, delete, archive, folders, tags.
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
  const [error, setError] = useState<string | null>(null);

  // Edit drawers
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [folderDraftId, setFolderDraftId] = useState<string | null>(null);
  const [folderDraft, setFolderDraft] = useState("");
  const [tagsDraftId, setTagsDraftId] = useState<string | null>(null);
  const [tagsDraft, setTagsDraft] = useState("");

  const refresh = useCallback(async () => {
    setError(null);
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
      setRows(list);
      setFolders(f);
      setAllTags(t);
    } catch (e) {
      setError(String(e));
    }
  }, [scope, kindFilter]);

  useEffect(() => {
    if (!open) return;
    void refresh();
  }, [open, refresh]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

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
      const hay = [
        s.title,
        s.cwd,
        s.folder ?? "",
        ...(s.tags ?? []),
      ]
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

  async function run(op: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await op();
      await refresh();
      onChanged();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function rename(id: string) {
    const title = renameDraft.trim();
    if (!title) return;
    await run(async () => {
      await api.sessionRename(id, title);
      setRenamingId(null);
    });
  }

  async function del(ids: string[]) {
    if (!ids.length) return;
    const ok = window.confirm(
      ids.length === 1
        ? "Delete this session permanently? Transcript is removed from disk."
        : `Delete ${ids.length} sessions permanently?`,
    );
    if (!ok) return;
    await run(async () => {
      for (const id of ids) {
        await api.sessionDelete(id);
      }
      setSelected(new Set());
    });
  }

  async function archive(ids: string[], archived: boolean) {
    await run(async () => {
      for (const id of ids) {
        await api.sessionArchive(id, archived);
      }
      setSelected(new Set());
    });
  }

  async function setFolder(id: string) {
    const folder = folderDraft.trim() || null;
    await run(async () => {
      await api.sessionSetFolder(id, folder);
      setFolderDraftId(null);
    });
  }

  async function setTags(id: string) {
    await run(async () => {
      await api.sessionSetTags(id, normalizeTags(tagsDraft));
      setTagsDraftId(null);
    });
  }

  async function bulkFolder(folder: string | null) {
    const ids = [...selected];
    if (!ids.length) return;
    await run(async () => {
      for (const id of ids) {
        await api.sessionSetFolder(id, folder);
      }
    });
  }

  if (!open) return null;

  return (
    <div className="session-browser" role="dialog" aria-modal="true" aria-label="Sessions">
      <header className="sb-header">
        <div className="sb-title-block">
          <h2>Sessions</h2>
          <span className="sb-sub">
            Rename · archive · folders · tags · delete
          </span>
        </div>
        <div className="sb-header-actions">
          <button type="button" className="primary" disabled={busy} onClick={() => void refresh()}>
            Refresh
          </button>
          <button type="button" onClick={onClose}>
            Close ⌘/Esc
          </button>
        </div>
      </header>

      <div className="sb-toolbar">
        <div className="sb-scopes" role="tablist">
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
              role="tab"
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
        <select
          value={kindFilter}
          onChange={(e) => {
            setKindFilter(e.target.value as KindFilter);
            setSelected(new Set());
          }}
          title="Chat vs build"
        >
          <option value="all">All kinds</option>
          <option value="chat">Chats</option>
          <option value="build">Builds</option>
        </select>
        <input
          className="sb-search"
          placeholder="Search title, path, folder, tags…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <select
          value={folderFilter}
          onChange={(e) => setFolderFilter(e.target.value)}
          title="Filter by folder"
        >
          <option value="">All folders</option>
          <option value="__inbox__">Inbox (no folder)</option>
          {folders.map((f) => (
            <option key={f} value={f}>
              {f}
            </option>
          ))}
        </select>
        <select
          value={tagFilter}
          onChange={(e) => setTagFilter(e.target.value)}
          title="Filter by tag"
        >
          <option value="">All tags</option>
          {allTags.map((t) => (
            <option key={t} value={t}>
              #{t}
            </option>
          ))}
        </select>
      </div>

      <div className="sb-bulk">
        <button type="button" disabled={!filtered.length} onClick={selectAllVisible}>
          Select visible ({filtered.length})
        </button>
        <button type="button" disabled={!selected.size} onClick={() => setSelected(new Set())}>
          Clear selection
        </button>
        <button
          type="button"
          disabled={!selected.size || busy}
          onClick={() => void archive([...selected], scope !== "archive")}
        >
          {scope === "archive" ? "Unarchive selected" : "Archive selected"}
        </button>
        <button
          type="button"
          className="danger"
          disabled={!selected.size || busy}
          onClick={() => void del([...selected])}
        >
          Delete selected
        </button>
        <select
          disabled={!selected.size || busy}
          defaultValue=""
          onChange={(e) => {
            const v = e.target.value;
            if (!v) return;
            void bulkFolder(v === "__clear__" ? null : v);
            e.target.value = "";
          }}
        >
          <option value="">Move selected to folder…</option>
          <option value="__clear__">Inbox (clear folder)</option>
          {folders.map((f) => (
            <option key={f} value={f}>
              {f}
            </option>
          ))}
        </select>
        {selected.size > 0 && (
          <span className="sb-sel-count">{selected.size} selected</span>
        )}
      </div>

      {error && <div className="sb-error">{error}</div>}

      <div className="sb-body">
        {byFolder.length === 0 && (
          <div className="sb-empty">No sessions match this view.</div>
        )}
        {byFolder.map(({ folder, items }) => (
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
                          checked={isSelected}
                          onChange={() => toggleSelect(s.id)}
                        />
                      </label>
                      <div className="sb-main">
                        {renamingId === s.id ? (
                          <form
                            className="sb-inline-form"
                            onSubmit={(e) => {
                              e.preventDefault();
                              void rename(s.id);
                            }}
                          >
                            <input
                              autoFocus
                              value={renameDraft}
                              onChange={(e) => setRenameDraft(e.target.value)}
                            />
                            <button type="submit" className="primary" disabled={busy}>
                              Save
                            </button>
                            <button type="button" onClick={() => setRenamingId(null)}>
                              Cancel
                            </button>
                          </form>
                        ) : (
                          <button
                            type="button"
                            className="sb-title-btn"
                            onClick={() => onOpen(s)}
                            title="Open session"
                          >
                            <span className={`sp-kind ${s.kind ?? "build"}`}>
                              {s.kind ?? "build"}
                            </span>
                            {s.title}
                            {isActive && <span className="sb-pill">open</span>}
                            {s.archived && <span className="sb-pill archive">archived</span>}
                          </button>
                        )}
                        <div className="sb-meta">
                          <span>{s.message_count} msgs</span>
                          <span>·</span>
                          <span title={s.updated_at}>{fmtDate(s.updated_at)}</span>
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
                                onClick={() => setTagFilter(t)}
                              >
                                #{t}
                              </button>
                            ))}
                          </div>
                        )}
                        {folderDraftId === s.id && (
                          <form
                            className="sb-inline-form"
                            onSubmit={(e) => {
                              e.preventDefault();
                              void setFolder(s.id);
                            }}
                          >
                            <input
                              autoFocus
                              list="sb-folder-suggestions"
                              placeholder="Folder name (empty = Inbox)"
                              value={folderDraft}
                              onChange={(e) => setFolderDraft(e.target.value)}
                            />
                            <datalist id="sb-folder-suggestions">
                              {folders.map((f) => (
                                <option key={f} value={f} />
                              ))}
                            </datalist>
                            <button type="submit" className="primary" disabled={busy}>
                              Save folder
                            </button>
                            <button type="button" onClick={() => setFolderDraftId(null)}>
                              Cancel
                            </button>
                          </form>
                        )}
                        {tagsDraftId === s.id && (
                          <form
                            className="sb-inline-form"
                            onSubmit={(e) => {
                              e.preventDefault();
                              void setTags(s.id);
                            }}
                          >
                            <input
                              autoFocus
                              placeholder="tags, comma separated"
                              value={tagsDraft}
                              onChange={(e) => setTagsDraft(e.target.value)}
                            />
                            <button type="submit" className="primary" disabled={busy}>
                              Save tags
                            </button>
                            <button type="button" onClick={() => setTagsDraftId(null)}>
                              Cancel
                            </button>
                          </form>
                        )}
                      </div>
                      <div className="sb-actions">
                        <button
                          type="button"
                          onClick={() => {
                            setRenamingId(s.id);
                            setRenameDraft(s.title);
                          }}
                        >
                          Rename
                        </button>
                        <button
                          type="button"
                          onClick={() => {
                            setFolderDraftId(s.id);
                            setFolderDraft(s.folder ?? "");
                          }}
                        >
                          Folder
                        </button>
                        <button
                          type="button"
                          onClick={() => {
                            setTagsDraftId(s.id);
                            setTagsDraft((s.tags ?? []).join(", "));
                          }}
                        >
                          Tags
                        </button>
                        <button
                          type="button"
                          disabled={busy}
                          onClick={() => void archive([s.id], !s.archived)}
                        >
                          {s.archived ? "Unarchive" : "Archive"}
                        </button>
                        <button
                          type="button"
                          className="danger"
                          disabled={busy}
                          onClick={() => void del([s.id])}
                        >
                          Delete
                        </button>
                        <button
                          type="button"
                          className="primary"
                          onClick={() => onOpen(s)}
                        >
                          Open
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
  );
}
