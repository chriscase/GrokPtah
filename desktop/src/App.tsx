import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "./lib/api";
import {
  normalizeSessionUpdate,
  SLASH_COMMANDS,
  type AgentStatus,
  type AuthState,
  type ModelInfo,
  type PermissionRequest,
  type SessionSummary,
  type SessionTab,
  type SessionUpdate,
  type TranscriptItem,
} from "./lib/protocol";
import { BrandMark } from "./components/BrandMark";
import {
  ContextMenu,
  type ContextMenuState,
} from "./components/ContextMenu";
import { FleetStrip } from "./components/FleetStrip";
import { SearchPanel } from "./components/SearchPanel";
import { SessionBrowser } from "./components/SessionBrowser";
import { SessionPane } from "./components/SessionPane";
import { SettingsPanel } from "./components/SettingsPanel";
import { TerminalPane, type ToolShellAttach } from "./components/TerminalPane";
import { canSplitSessions, useLayoutDensity } from "./lib/layout";
import {
  collapseRepeatedText,
  mergeAssistantChunk,
  subscribeSessionUpdates,
} from "./lib/sessionEvents";
import {
  doneActivity,
  errorActivity,
  idleActivity,
  phaseFromPermission,
  phaseFromStream,
  phaseFromThought,
  phaseFromTool,
  queuedActivity,
  type ActivityState,
} from "./lib/activity";

type WorkspaceMode = "build" | "chat";

type RightTab =
  | "files"
  | "git"
  | "mcp"
  | "plugins"
  | "skills"
  | "tasks"
  | "rules";

function emptyTab(id: string, title = "New session"): SessionTab {
  return {
    id,
    title,
    transcript: [],
    busy: false,
    plan: null,
    activity: idleActivity(),
    unseen: false,
    needsPermission: false,
  };
}

function withActivity(
  tab: SessionTab,
  patch: Partial<ActivityState> & Pick<ActivityState, "phase" | "label">,
): SessionTab {
  const at = Date.now();
  const background =
    activeSessionIdForEvents != null && tab.id !== activeSessionIdForEvents;
  const permission = patch.phase === "permission";
  const cleared =
    patch.phase === "done" || patch.phase === "idle" || patch.phase === "error";
  return {
    ...tab,
    activity: {
      ...tab.activity,
      ...patch,
      lastEventAt: at,
      live: patch.live ?? tab.activity.live,
    },
    // Background tabs light up when anything arrives for them.
    unseen: background && (patch.live || permission) ? true : tab.unseen,
    needsPermission: permission
      ? true
      : cleared
        ? false
        : tab.needsPermission,
  };
}

/** Ref used by applyUpdate so background tabs can be marked unseen. */
let activeSessionIdForEvents: string | null = null;

/**
 * After a turn completes, ensure we show each assistant reply once.
 * Streamed events and the sessionPrompt return value are the same text;
 * without this, races leave two identical assistant bubbles.
 */
function finalizeTurnTranscript(
  transcript: TranscriptItem[],
  reply: string | null | undefined,
): TranscriptItem[] {
  let lastUser = -1;
  for (let i = transcript.length - 1; i >= 0; i--) {
    if (transcript[i].kind === "user") {
      lastUser = i;
      break;
    }
  }
  const head = lastUser >= 0 ? transcript.slice(0, lastUser + 1) : [];
  const tail =
    lastUser >= 0 ? transcript.slice(lastUser + 1) : [...transcript];

  // Strip any assistant bubbles from this turn's tail (they may be N× glued
  // copies from multi-listener events). The invoke return is the single
  // source of truth for the final assistant text.
  const nonAssistant: TranscriptItem[] = collapseAdjacentDuplicateAssistants(
    tail,
  )
    .filter((item) => item.kind !== "assistant")
    .map((item) =>
      item.kind === "thought"
        ? { kind: "thought" as const, text: item.text, streaming: false }
        : item,
    );

  const trimmed = reply?.trim() ? collapseRepeatedText(reply.trim()) : "";
  if (trimmed) {
    return [
      ...head,
      ...nonAssistant,
      { kind: "assistant", text: trimmed, streaming: false },
    ];
  }
  return [...head, ...nonAssistant];
}

/** Merge consecutive assistant bubbles that are the same (or prefix/suffix). */
function collapseAdjacentDuplicateAssistants(
  items: TranscriptItem[],
): TranscriptItem[] {
  const out: TranscriptItem[] = [];
  for (const item of items) {
    const prev = out[out.length - 1];
    if (item.kind === "assistant" && prev?.kind === "assistant") {
      const a = prev.text.trim();
      const b = item.text.trim();
      if (!a) {
        out[out.length - 1] = { ...item, streaming: false };
        continue;
      }
      if (!b || a === b || b.startsWith(a) || a.startsWith(b)) {
        const text = b.length >= a.length ? item.text : prev.text;
        out[out.length - 1] = {
          kind: "assistant",
          text,
          streaming: false,
        };
        continue;
      }
    }
    out.push(item);
  }
  return out;
}

/** Shorten a filesystem path for chrome (prefer last two segments). */
function shortPath(path: string | null | undefined, max = 42): string {
  if (!path) return "Set working directory…";
  let p = path;
  // Collapse home when it matches a typical macOS/Linux home prefix.
  const homeMatch = p.match(/^(\/Users\/[^/]+|\/home\/[^/]+)/);
  if (homeMatch) {
    p = `~${p.slice(homeMatch[1].length)}`;
  }
  if (p.length <= max) return p;
  const parts = p.split(/[/\\]/).filter(Boolean);
  if (parts.length <= 2) return p;
  return `…/${parts.slice(-2).join("/")}`;
}

export default function App() {
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [auth, setAuth] = useState<AuthState>({ signed_in: false });
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  /** Open concurrent workspaces (tabs). Multiple can be busy at once. */
  const [tabs, setTabs] = useState<SessionTab[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [composer, setComposer] = useState("");
  const [ctxMenu, setCtxMenu] = useState<ContextMenuState | null>(null);
  const [permission, setPermission] = useState<PermissionRequest | null>(null);
  const [rightTab, setRightTab] = useState<RightTab>("files");
  const [files, setFiles] = useState<string[]>([]);
  const [fuzzy, setFuzzy] = useState("");
  const [fuzzyHits, setFuzzyHits] = useState<string[]>([]);
  const [gitStatus, setGitStatus] = useState("");
  const [gitDiff, setGitDiff] = useState("");
  const [worktrees, setWorktrees] = useState("");
  const [mcp, setMcp] = useState<any[]>([]);
  const [mcpDoctor, setMcpDoctor] = useState<string[]>([]);
  const [plugins, setPlugins] = useState<any[]>([]);
  const [skills, setSkills] = useState<any[]>([]);
  const [subagents, setSubagents] = useState<any[]>([]);
  const [bgTasks, setBgTasks] = useState<any[]>([]);
  const [hooksPreview, setHooksPreview] = useState<string | null>(null);
  const [rules, setRules] = useState<string[]>([]);
  const [product, setProduct] = useState({
    name: "GrokPtah",
    bridgeVersion: "?",
    autoUpdateEnabled: false,
  });
  const [showTerm, setShowTerm] = useState(false);
  const [toolShell, setToolShell] = useState<ToolShellAttach | null>(null);
  const [aboutOpen, setAboutOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  /** False until we finish reopening tabs from ~/.grokptah/workspace.json. */
  const [workspaceRestored, setWorkspaceRestored] = useState(false);
  const [sessionBrowserOpen, setSessionBrowserOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  /** Secondary session column on wide layouts (Phase 14 split view). */
  const [sideSessionId, setSideSessionId] = useState<string | null>(null);
  const layoutDensity = useLayoutDensity();
  const splitOk = canSplitSessions(layoutDensity);
  const [workspaceMode, setWorkspaceMode] = useState<WorkspaceMode>("build");

  // Keep event router aware of focus for unseen badges.
  activeSessionIdForEvents = activeSessionId;

  const activeTab = useMemo(
    () => tabs.find((t) => t.id === activeSessionId) ?? null,
    [tabs, activeSessionId],
  );

  // Focusing a tab clears its unseen badge.
  useEffect(() => {
    if (!activeSessionId) return;
    setTabs((prev) =>
      prev.map((t) =>
        t.id === activeSessionId
          ? { ...t, unseen: false, needsPermission: t.activity.phase === "permission" }
          : t,
      ),
    );
  }, [activeSessionId]);
  const busy = activeTab?.busy ?? false;
  const plan = activeTab?.plan ?? null;
  const anyBusy = tabs.some((t) => t.busy);
  const activity = activeTab?.activity ?? idleActivity();
  const activeSummary = useMemo(
    () => sessions.find((s) => s.id === activeSessionId) ?? null,
    [sessions, activeSessionId],
  );
  const activeIsBuild =
    (activeSummary?.kind ?? workspaceMode) === "build";

  const patchTab = useCallback(
    (id: string, patch: (tab: SessionTab) => SessionTab) => {
      setTabs((prev) =>
        prev.map((t) => (t.id === id ? patch(t) : t)),
      );
    },
    [],
  );

  const openTab = useCallback(
    async (summary: SessionSummary, hydrate = true) => {
      setActiveSessionId(summary.id);
      setTabs((prev) => {
        if (prev.some((t) => t.id === summary.id)) {
          return prev.map((t) =>
            t.id === summary.id ? { ...t, title: summary.title } : t,
          );
        }
        return [
          ...prev,
          emptyTab(summary.id, summary.title || "New session"),
        ];
      });
      if (!hydrate) return;
      try {
        const entries = await api.sessionTranscript(summary.id);
        setTabs((prev) =>
          prev.map((t) => {
            if (t.id !== summary.id) return t;
            // Keep live stream if this tab already has more than disk.
            if (t.transcript.length > entries.length) return t;
            return {
              ...t,
              title: summary.title,
              transcript: entries.map((e) =>
                e.role === "user"
                  ? ({ kind: "user" as const, text: e.text })
                  : ({ kind: "assistant" as const, text: e.text }),
              ),
            };
          }),
        );
      } catch {
        /* offline / empty */
      }
    },
    [],
  );

  const closeTab = useCallback((id: string) => {
    setSideSessionId((side) => (side === id ? null : side));
    setTabs((prev) => {
      const next = prev.filter((t) => t.id !== id);
      setActiveSessionId((cur) => {
        if (cur !== id) return cur;
        return next[next.length - 1]?.id ?? null;
      });
      return next;
    });
  }, []);

  const openBeside = useCallback(
    async (sessionId: string) => {
      if (!splitOk) return;
      if (sessionId === activeSessionId) {
        const other = tabs.find((t) => t.id !== sessionId);
        if (other) setSideSessionId(other.id);
        return;
      }
      let summary = sessions.find((s) => s.id === sessionId);
      if (!summary) {
        try {
          summary = await api.sessionLoad(sessionId);
        } catch {
          return;
        }
      }
      const keepFocus = activeSessionId;
      const already = tabs.some((t) => t.id === sessionId);
      await openTab(summary, !already);
      // openTab focuses the opened session — restore primary focus.
      if (keepFocus) setActiveSessionId(keepFocus);
      setSideSessionId(sessionId);
    },
    [splitOk, activeSessionId, tabs, sessions, openTab],
  );

  // Drop side pane when the window is too narrow for split.
  useEffect(() => {
    if (!splitOk) setSideSessionId(null);
  }, [splitOk]);

  // ⌘\ / Ctrl+\ — open another open tab beside the focused session.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.key !== "\\") return;
      e.preventDefault();
      if (!splitOk || !activeSessionId) return;
      if (sideSessionId) {
        setSideSessionId(null);
        return;
      }
      const other = tabs.find((t) => t.id !== activeSessionId);
      if (other) void openBeside(other.id);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [splitOk, activeSessionId, sideSessionId, tabs, openBeside]);

  const refreshSessions = useCallback(async () => {
    try {
      setSessions(await api.sessionListByKind(workspaceMode, false));
    } catch {
      /* bridge down */
    }
  }, [workspaceMode]);

  useEffect(() => {
    if (!workspaceRestored) return;
    void refreshSessions();
  }, [workspaceMode, workspaceRestored, refreshSessions]);

  /** Open from browser or sidebar; drop from tabs if deleted. */
  const handleSessionBrowserOpen = useCallback(
    async (s: SessionSummary) => {
      if (s.archived) {
        // Unarchive when opening so it re-enters the active list.
        try {
          s = await api.sessionArchive(s.id, false);
        } catch {
          /* keep trying load */
        }
      }
      if (s.kind === "chat" || s.kind === "build") {
        setWorkspaceMode(s.kind);
      }
      await api.sessionLoad(s.id);
      await openTab(s, true);
      setSessionBrowserOpen(false);
      await refreshSessions();
    },
    [openTab, refreshSessions],
  );

  const handleSessionBrowserChanged = useCallback(async () => {
    await refreshSessions();
    // Drop tabs for deleted / archived sessions
    try {
      const live = await api.sessionListAll();
      const liveIds = new Set(live.map((s) => s.id));
      const archivedIds = new Set(
        live.filter((s) => s.archived).map((s) => s.id),
      );
      setTabs((prev) =>
        prev.filter((t) => liveIds.has(t.id) && !archivedIds.has(t.id)),
      );
      setActiveSessionId((cur) => {
        if (!cur) return cur;
        if (!liveIds.has(cur) || archivedIds.has(cur)) {
          return null;
        }
        return cur;
      });
      // Refresh tab titles after rename
      setTabs((prev) =>
        prev.map((t) => {
          const s = live.find((x) => x.id === t.id);
          return s ? { ...t, title: s.title } : t;
        }),
      );
    } catch {
      /* ignore */
    }
  }, [refreshSessions]);

  const refreshChrome = useCallback(async () => {
    try {
      const [st, au, md, sess, info] = await Promise.all([
        api.agentStatus(),
        api.authState(),
        api.listModels(),
        api.sessionList(),
        api.productInfo(),
      ]);
      setStatus(st);
      setAuth(au);
      setModels(md);
      setSessions(sess);
      setProduct(info);
      // Keep tab titles in sync with session list
      setTabs((prev) =>
        prev.map((t) => {
          const s = sess.find((x) => x.id === t.id);
          return s ? { ...t, title: s.title } : t;
        }),
      );
    } catch (e) {
      console.warn("refresh failed (browser-only?)", e);
    }
  }, []);

  // Chrome refresh on mount only (not tied to the event listener).
  useEffect(() => {
    void refreshChrome();
  }, [refreshChrome]);

  // Page-lifetime singleton bus (survives StrictMode + Vite HMR). Raw
  // listen() per mount stacked handlers and glued the same reply N times.
  useEffect(() => {
    return subscribeSessionUpdates((u) => {
      if (u.type === "shell_session_started") {
        setShowTerm(true);
        setToolShell({ callId: u.call_id, command: u.command });
      }
      applyUpdate(u, setTabs, setPermission);
    });
  }, []);

  // Restore sessions + open tabs from disk (desktop-app durability).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        try {
          await api.agentStart();
        } catch {
          /* already running from Tauri setup */
        }
        const ws = await api.workspaceState();
        if (cancelled) return;
        setSessions(ws.sessions);
        const byId = new Map(ws.sessions.map((s) => [s.id, s]));
        // Drop tabs for sessions that no longer exist on disk (test garbage, deletes).
        let tabIds = (ws.open_tab_ids ?? []).filter((id) => byId.has(id));
        if (tabIds.length === 0) {
          tabIds = ws.sessions
            .filter((s) => s.message_count > 0)
            .slice(0, 8)
            .map((s) => s.id);
        }
        // Persist the prune so a deleted/missing session doesn't keep reappearing.
        if (
          tabIds.length !== (ws.open_tab_ids ?? []).length ||
          (ws.active_session && !byId.has(ws.active_session))
        ) {
          try {
            await api.setOpenTabs(
              tabIds,
              ws.active_session && byId.has(ws.active_session)
                ? ws.active_session
                : (tabIds[0] ?? null),
            );
          } catch {
            /* ignore */
          }
        }
        for (const id of tabIds) {
          const summary = byId.get(id);
          if (!summary) continue;
          await openTab(summary, true);
        }
        const active =
          (ws.active_session && byId.has(ws.active_session)
            ? ws.active_session
            : null) ??
          tabIds[0] ??
          null;
        if (active) {
          setActiveSessionId(active);
          const activeSummary = byId.get(active);
          if (activeSummary?.kind === "chat" || activeSummary?.kind === "build") {
            setWorkspaceMode(activeSummary.kind);
          }
          try {
            await api.sessionLoad(active);
          } catch {
            /* missing */
          }
        }
        if (ws.project_cwd) {
          // status refresh will surface path; host already loaded it
          await refreshChrome();
        }
      } catch (e) {
        console.warn("workspace restore failed", e);
      } finally {
        if (!cancelled) setWorkspaceRestored(true);
      }
    })();
    return () => {
      cancelled = true;
    };
    // openTab/refreshChrome are stable enough; run once on mount
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Persist tab strip whenever it changes (after initial restore).
  useEffect(() => {
    if (!workspaceRestored) return;
    void api.setOpenTabs(
      tabs.map((t) => t.id),
      activeSessionId,
    );
  }, [tabs, activeSessionId, workspaceRestored]);

  const slashOpen = composer.startsWith("/") && !composer.includes(" ");
  const slashHits = useMemo(
    () =>
      SLASH_COMMANDS.filter((c) =>
        c.cmd.startsWith(composer || "/"),
      ),
    [composer],
  );

  async function openProject() {
    const path = await api.pickProjectFolder();
    if (path) {
      // Also pin the folder on the active build so tools use it.
      if (
        activeSessionId &&
        (activeSummary?.kind ?? workspaceMode) === "build"
      ) {
        try {
          await api.sessionSetCwd(activeSessionId, path);
        } catch {
          /* host cwd still set by pick */
        }
      }
      await refreshChrome();
      await refreshSessions();
      try {
        setFiles(await api.fileTree());
      } catch {
        /* empty */
      }
    }
  }

  const setWorkingDirectory = useCallback(
    async (sessionId: string) => {
      try {
        const updated = await api.pickSessionFolder(sessionId);
        if (!updated) return;
        await refreshSessions();
        await refreshChrome();
        try {
          setFiles(await api.fileTree());
        } catch {
          /* no project / empty */
        }
      } catch (e) {
        console.warn("set working directory failed", e);
      }
    },
    [refreshSessions, refreshChrome],
  );

  const createSession = useCallback(
    async (kind: WorkspaceMode) => {
      // Builds need a project root; offer the folder picker if none is set yet.
      if (kind === "build" && !status?.project_cwd) {
        const path = await api.pickProjectFolder();
        if (!path) return null;
        await refreshChrome();
      }
      const s = await api.sessionNewKind(kind);
      await openTab(s, false);
      await refreshSessions();
      if (kind === "build") {
        try {
          setFiles(await api.fileTree());
        } catch {
          /* empty */
        }
      }
      return s;
    },
    [status?.project_cwd, openTab, refreshSessions, refreshChrome],
  );

  async function ensureSession(): Promise<string> {
    // Prefer the active tab only when its kind matches Builds/Chats mode.
    // Otherwise a build tab left open would answer "chat" prompts as kind=build.
    if (activeSessionId) {
      const fromList = sessions.find((s) => s.id === activeSessionId);
      let kind = fromList?.kind;
      if (!kind) {
        try {
          const loaded = await api.sessionLoad(activeSessionId);
          kind = loaded.kind;
        } catch {
          // Missing/deleted session (e.g. cleaned test garbage) — create fresh.
          kind = undefined;
        }
      }
      if (kind === workspaceMode) {
        return activeSessionId;
      }
    }
    // Reuse any already-open tab of the right kind.
    const openMatch = tabs
      .map((t) => sessions.find((s) => s.id === t.id))
      .find((s) => s && (s.kind ?? "build") === workspaceMode);
    if (openMatch) {
      setActiveSessionId(openMatch.id);
      try {
        await api.sessionLoad(openMatch.id);
      } catch {
        /* ignore */
      }
      return openMatch.id;
    }
    const s = await createSession(workspaceMode);
    if (!s) throw new Error("session creation cancelled");
    return s.id;
  }

  const openSessionContextMenu = useCallback(
    (sessionId: string, x: number, y: number) => {
      const current = sessions.find((s) => s.id === sessionId);
      const isBuild =
        (current?.kind ?? workspaceMode) === "build";
      setCtxMenu({
        x,
        y,
        items: [
          {
            type: "item",
            id: "rename",
            label: "Rename…",
            onClick: () => {
              void (async () => {
                const next = window.prompt(
                  "Rename session",
                  current?.title ?? "",
                );
                if (next == null || !next.trim()) return;
                const updated = await api.sessionRename(
                  sessionId,
                  next.trim(),
                );
                patchTab(sessionId, (t) => ({ ...t, title: updated.title }));
                setSessions(await api.sessionListByKind(workspaceMode, false));
              })();
            },
          },
          {
            type: "item",
            id: "beside",
            label: splitOk ? "Open beside" : "Open beside (need wider window)",
            disabled: !splitOk,
            onClick: () => {
              void openBeside(sessionId);
            },
          },
          ...(isBuild
            ? ([
                {
                  type: "item" as const,
                  id: "cwd",
                  label: "Set working directory…",
                  onClick: () => {
                    void setWorkingDirectory(sessionId);
                  },
                },
              ] as const)
            : []),
          {
            type: "item",
            id: "fork",
            label: "Fork",
            onClick: () => {
              void (async () => {
                const f = await api.sessionFork(sessionId);
                await openTab(f, false);
                const src = tabs.find((t) => t.id === sessionId);
                if (src) {
                  patchTab(f.id, (t) => ({
                    ...t,
                    transcript: [...src.transcript],
                    title: f.title,
                  }));
                }
                setSessions(
                  await api.sessionListByKind(workspaceMode, false),
                );
              })();
            },
          },
          {
            type: "item",
            id: "rewind",
            label: "Rewind last message",
            onClick: () => {
              void (async () => {
                await api.sessionRewind(sessionId, 1);
                const list = await api.sessionListByKind(
                  workspaceMode,
                  false,
                );
                setSessions(list);
                const summary = list.find((s) => s.id === sessionId);
                if (summary) await openTab(summary, true);
              })();
            },
          },
          {
            type: "item",
            id: "compact",
            label: "Compact server context",
            onClick: () => {
              void (async () => {
                await api.sessionCompact(sessionId);
                try {
                  const entries = await api.sessionTranscript(sessionId);
                  patchTab(sessionId, (t) => ({
                    ...t,
                    transcript: entries.map((e) =>
                      e.role === "user"
                        ? ({ kind: "user" as const, text: e.text })
                        : ({ kind: "assistant" as const, text: e.text }),
                    ),
                  }));
                } catch {
                  /* keep local view */
                }
              })();
            },
          },
          { type: "separator" },
          {
            type: "item",
            id: "archive",
            label: "Archive",
            onClick: () => {
              void (async () => {
                await api.sessionArchive(sessionId, true);
                closeTab(sessionId);
                setSessions(
                  await api.sessionListByKind(workspaceMode, false),
                );
              })();
            },
          },
          {
            type: "item",
            id: "delete",
            label: "Delete permanently…",
            danger: true,
            onClick: () => {
              void (async () => {
                if (
                  !window.confirm(
                    "Delete this session permanently? Transcript is removed from disk.",
                  )
                ) {
                  return;
                }
                await api.sessionDelete(sessionId);
                closeTab(sessionId);
                setSessions(
                  await api.sessionListByKind(workspaceMode, false),
                );
              })();
            },
          },
          { type: "separator" },
          {
            type: "item",
            id: "browse",
            label: "Browse all sessions…",
            onClick: () => setSessionBrowserOpen(true),
          },
        ],
      });
    },
    [
      sessions,
      tabs,
      workspaceMode,
      openTab,
      closeTab,
      patchTab,
      setWorkingDirectory,
      splitOk,
      openBeside,
    ],
  );

  async function sendPrompt(text?: string) {
    const prompt = (text ?? composer).trim();
    if (!prompt) return;
    setComposer("");
    let id: string;
    try {
      id = await ensureSession();
    } catch (e) {
      console.warn(e);
      return;
    }
    patchTab(id, (t) => ({
      ...t,
      busy: true,
      activity: queuedActivity(),
      title:
        t.title === "New session" || t.title === "New chat"
          ? prompt.slice(0, 48)
          : t.title,
      transcript: [...t.transcript, { kind: "user", text: prompt }],
    }));
    try {
      if (prompt === "/compact") {
        await api.sessionCompact(id);
        // Compact only shrinks the server context window — rehydrate full
        // local history so nothing appears deleted in the UI.
        try {
          const entries = await api.sessionTranscript(id);
          patchTab(id, (t) => ({
            ...t,
            busy: false,
            activity: doneActivity(false),
            transcript: entries.map((e) =>
              e.role === "user"
                ? ({ kind: "user" as const, text: e.text })
                : ({
                    kind: "assistant" as const,
                    text: e.text,
                  }),
            ),
          }));
        } catch {
          patchTab(id, (t) => ({
            ...t,
            busy: false,
            activity: doneActivity(false),
            transcript: [
              ...t.transcript,
              {
                kind: "assistant",
                text: "Context compacted for the server. Full local history is retained.",
              },
            ],
          }));
        }
        return;
      }
      const reply = await api.sessionPrompt(id, prompt);
      // Events stream the assistant text AND the invoke returns the same
      // string. React state from events can still be pending when we get
      // here, so naive "if not already" checks append a second identical
      // bubble. Always finalize the turn: collapse duplicate assistants
      // after the last user message; only append reply if none exist.
      patchTab(id, (t) => ({
        ...t,
        busy: false,
        activity: doneActivity(false),
        transcript: finalizeTurnTranscript(t.transcript, reply),
      }));
      setSubagents(await api.subagentsList());
      setBgTasks(await api.backgroundTasks());
      await refreshChrome();
      await refreshSessions();
    } catch (e) {
      patchTab(id, (t) => ({
        ...t,
        busy: false,
        activity: errorActivity(String(e)),
        transcript: [
          ...t.transcript,
          { kind: "error", text: String(e) },
        ],
      }));
    }
  }

  async function loadRight(tab: RightTab) {
    setRightTab(tab);
    try {
      if (tab === "files") setFiles(await api.fileTree());
      if (tab === "git") {
        setGitStatus(await api.gitStatus());
        setGitDiff(await api.gitDiff());
        setWorktrees(await api.listWorktrees());
      }
      if (tab === "mcp") {
        setMcp(await api.mcpList());
        setMcpDoctor(await api.mcpDoctor());
      }
      if (tab === "plugins") setPlugins(await api.pluginsList());
      if (tab === "skills") setSkills(await api.skillsList());
      if (tab === "tasks") {
        setSubagents(await api.subagentsList());
        setBgTasks(await api.backgroundTasks());
      }
      if (tab === "rules") setRules(await api.projectRules());
    } catch (e) {
      console.warn(e);
    }
  }

  async function onFuzzy(q: string) {
    setFuzzy(q);
    if (!q) {
      setFuzzyHits([]);
      return;
    }
    try {
      setFuzzyHits(await api.fuzzyOpen(q));
    } catch {
      setFuzzyHits([]);
    }
  }

  return (
    <div className="app-shell">
      <header className="titlebar">
        <div className="brand">
          <BrandMark size={20} className="brand-mark-img" />
          <span className="brand-name">
            GrokPtah
            <span className="brand-tag"> · coding agent</span>
          </span>
        </div>
        <div className="title-actions">
          <span className="path-chip" title={status?.project_cwd ?? ""}>
            {status?.project_cwd
              ? shortPath(status.project_cwd, 36)
              : "no project open"}
          </span>
          <button type="button" onClick={() => void openProject()}>
            Open folder
          </button>
          <button
            type="button"
            className={`auth-chip ${auth.signed_in ? "on" : ""}`}
            title={
              auth.signed_in
                ? `${auth.display_name} (${auth.method}) — open Settings`
                : "Not signed in — open Settings to authenticate"
            }
            onClick={() => setSettingsOpen(true)}
          >
            {auth.signed_in ? auth.method : "auth: none"}
          </button>
          <button
            type="button"
            className="icon-btn"
            title="Settings"
            aria-label="Open settings"
            onClick={() => setSettingsOpen(true)}
          >
            <svg width="15" height="15" viewBox="0 0 16 16" fill="none" aria-hidden>
              <path
                d="M6.5 2.5h3l.4 1.6a4.5 4.5 0 0 1 1.1.6l1.6-.5.9 1.5-1.2 1.1c.1.3.1.7.1 1s0 .7-.1 1l1.2 1.1-.9 1.5-1.6-.5a4.5 4.5 0 0 1-1.1.6L9.5 13.5h-3l-.4-1.6a4.5 4.5 0 0 1-1.1-.6l-1.6.5-.9-1.5 1.2-1.1A4 4 0 0 1 3.6 8c0-.3 0-.7.1-1L2.5 5.9l.9-1.5 1.6.5c.3-.3.7-.5 1.1-.6L6.5 2.5Z"
                stroke="currentColor"
                strokeWidth="1.2"
                strokeLinejoin="round"
              />
              <circle
                cx="8"
                cy="8"
                r="1.75"
                stroke="currentColor"
                strokeWidth="1.2"
              />
            </svg>
          </button>
        </div>
      </header>

      <aside className="sidebar">
        <div className="section-title">Workspace</div>
        <div
          className="workspace-mode"
          data-mode={workspaceMode}
          role="tablist"
          aria-label="Workspace mode"
        >
          <button
            type="button"
            className={workspaceMode === "build" ? "active" : ""}
            onClick={() => {
              setWorkspaceMode("build");
              // Don't keep answering in a chat tab while Builds is selected.
              const buildTab = tabs.find((t) => {
                const s = sessions.find((x) => x.id === t.id);
                return (s?.kind ?? "build") === "build";
              });
              if (buildTab) setActiveSessionId(buildTab.id);
              else if (
                activeSummary &&
                (activeSummary.kind ?? "build") !== "build"
              ) {
                setActiveSessionId(null);
              }
            }}
          >
            Builds
          </button>
          <button
            type="button"
            className={workspaceMode === "chat" ? "active" : ""}
            onClick={() => {
              setWorkspaceMode("chat");
              const chatTab = tabs.find((t) => {
                const s = sessions.find((x) => x.id === t.id);
                return s?.kind === "chat";
              });
              if (chatTab) setActiveSessionId(chatTab.id);
              else if (activeSummary?.kind !== "chat") {
                // Force ensureSession to create a chat on next send.
                setActiveSessionId(null);
              }
            }}
          >
            Chats
          </button>
        </div>
        <div className="sidebar-actions">
          <button
            type="button"
            className="primary sidebar-action-primary"
            onClick={() => void createSession(workspaceMode)}
          >
            {workspaceMode === "chat" ? "New chat" : "New build"}
          </button>
          <div className="sidebar-action-row">
            <button
              type="button"
              className="sidebar-action-ghost"
              onClick={() => setSearchOpen(true)}
              title="Search sessions"
            >
              Search
            </button>
            <button
              type="button"
              className="sidebar-action-ghost"
              onClick={() => setSessionBrowserOpen(true)}
              title="Browse all sessions"
            >
              Browse
            </button>
          </div>
        </div>
        <p className="session-hint">
          {workspaceMode === "chat"
            ? "Grok chats — separate from coding builds."
            : "Coding builds with tools."}
          <span className="session-hint-ctx">
            {splitOk
              ? " Right-click → Open beside · ⌘\\ toggles split."
              : " Right-click a session for actions. Widen for side-by-side."}
          </span>
        </p>
        <div className="session-list">
          {sessions.map((s) => {
            const open = tabs.some((t) => t.id === s.id);
            const tabMeta = tabs.find((t) => t.id === s.id);
            const running = tabMeta?.busy;
            return (
              <div
                key={s.id}
                className={`session-item ${s.id === activeSessionId ? "active" : ""} ${running ? "busy" : ""} ${tabMeta?.needsPermission ? "needs-permission" : ""} ${tabMeta?.unseen ? "has-unseen" : ""}`}
                onContextMenu={(e) => {
                  e.preventDefault();
                  openSessionContextMenu(s.id, e.clientX, e.clientY);
                }}
              >
                <button
                  type="button"
                  className="session-item-main"
                  onClick={async () => {
                    await api.sessionLoad(s.id);
                    await openTab(s, !open);
                  }}
                  onDoubleClick={() => setSessionBrowserOpen(true)}
                >
                  <div className="session-item-title">
                    {tabMeta?.needsPermission ? (
                      <span
                        className="attn-dot permission"
                        title="Needs your response"
                      />
                    ) : running ? (
                      <span className="busy-dot" title="Running" />
                    ) : tabMeta?.unseen ? (
                      <span
                        className="attn-dot unseen"
                        title="Unseen activity"
                      />
                    ) : null}
                    <span className={`kind-chip ${s.kind ?? workspaceMode}`}>
                      {s.kind ?? workspaceMode}
                    </span>
                    <span className="session-item-name">{s.title}</span>
                  </div>
                  <div className="session-item-sub">
                    {s.message_count} msgs{open ? " · open" : ""}
                    {(s.kind ?? workspaceMode) === "build" && s.cwd
                      ? ` · ${shortPath(s.cwd, 28)}`
                      : ""}
                  </div>
                  {(s.folder || (s.tags?.length ?? 0) > 0) && (
                    <div className="session-side-meta">
                      {s.folder && <span>▣ {s.folder}</span>}
                      {(s.tags ?? []).slice(0, 3).map((t) => (
                        <span key={t}>#{t}</span>
                      ))}
                    </div>
                  )}
                </button>
                <button
                  type="button"
                  className="session-item-more"
                  aria-label={`Actions for ${s.title}`}
                  title="Session actions"
                  onClick={(e) => {
                    e.stopPropagation();
                    const r = e.currentTarget.getBoundingClientRect();
                    openSessionContextMenu(s.id, r.right, r.bottom + 2);
                  }}
                >
                  ···
                </button>
              </div>
            );
          })}
        </div>
      </aside>

      <main
        className={`main density-${layoutDensity} ${
          splitOk && sideSessionId ? "is-split" : ""
        }`}
      >
        {tabs.length > 0 && (
          <div className="session-tabs" role="tablist" aria-label="Open sessions">
            {tabs.map((t) => (
              <div
                key={t.id}
                className={`session-tab ${t.id === activeSessionId ? "active" : ""} ${t.id === sideSessionId ? "is-side" : ""} ${t.busy ? "busy" : ""} ${t.needsPermission ? "needs-permission" : ""} ${t.unseen ? "has-unseen" : ""}`}
                role="tab"
                aria-selected={t.id === activeSessionId}
                onContextMenu={(e) => {
                  e.preventDefault();
                  openSessionContextMenu(t.id, e.clientX, e.clientY);
                }}
              >
                <button
                  type="button"
                  className="session-tab-label"
                  onClick={() => setActiveSessionId(t.id)}
                  title={t.title}
                >
                  {t.needsPermission ? (
                    <span
                      className="attn-dot permission"
                      title="Needs your response"
                    />
                  ) : t.busy ? (
                    <span className="busy-dot" title="Working" />
                  ) : t.unseen ? (
                    <span className="attn-dot unseen" title="Unseen activity" />
                  ) : null}
                  <span className="session-tab-text">{t.title}</span>
                </button>
                <button
                  type="button"
                  className="session-tab-close"
                  aria-label={`Close ${t.title}`}
                  onClick={(e) => {
                    e.stopPropagation();
                    closeTab(t.id);
                  }}
                >
                  ×
                </button>
              </div>
            ))}
            <button
              type="button"
              className="session-tab-new"
              title={`New ${workspaceMode} tab`}
              onClick={() => void createSession(workspaceMode)}
            >
              +
            </button>
            {splitOk && tabs.length >= 2 && (
              <button
                type="button"
                className={`session-tab-split ${sideSessionId ? "on" : ""}`}
                title={
                  sideSessionId
                    ? "Close side pane (⌘\\)"
                    : "Open another tab beside (⌘\\)"
                }
                onClick={() => {
                  if (sideSessionId) {
                    setSideSessionId(null);
                    return;
                  }
                  const other = tabs.find((t) => t.id !== activeSessionId);
                  if (other) void openBeside(other.id);
                }}
              >
                ⧉
              </button>
            )}
          </div>
        )}
        {activeIsBuild && activeSessionId && (
          <div className="session-cwd-bar">
            <button
              type="button"
              className="session-cwd-btn"
              title={
                activeSummary?.cwd
                  ? `${activeSummary.cwd}\nClick to change working directory`
                  : "Choose a working directory for this build"
              }
              onClick={() => void setWorkingDirectory(activeSessionId)}
            >
              <span className="session-cwd-label">cwd</span>
              <span className="session-cwd-path">
                {shortPath(activeSummary?.cwd)}
              </span>
              <span className="session-cwd-change">Change</span>
            </button>
          </div>
        )}

        <div
          className={`pane-row ${
            splitOk && sideSessionId ? "has-side" : "single"
          }`}
        >
          {activeTab ? (
            <SessionPane
              tab={activeTab}
              focused
              kindLabel={
                sessions.find((s) => s.id === activeTab.id)?.kind ??
                workspaceMode
              }
              bridgeVersion={product.bridgeVersion}
              emptyHint={
                workspaceMode === "build"
                  ? "Set a working directory, then send a prompt."
                  : "Message Grok when this pane is focused."
              }
              onFocus={() => setActiveSessionId(activeTab.id)}
            />
          ) : (
            <div className="transcript session-pane-transcript">
              <div className="empty-agent">
                <h1>GrokPtah</h1>
                <div className="version-line">
                  bridge {product.bridgeVersion}
                </div>
                <ul>
                  <li>
                    New build / chat from the sidebar, or open an existing
                    session
                  </li>
                  <li>
                    {splitOk
                      ? "Wide layout: right-click → Open beside for split view"
                      : "Widen the window (≥1440px) for side-by-side sessions"}
                  </li>
                </ul>
              </div>
            </div>
          )}
          {splitOk &&
            sideSessionId &&
            (() => {
              const sideTab = tabs.find((t) => t.id === sideSessionId);
              if (!sideTab) return null;
              return (
                <SessionPane
                  tab={sideTab}
                  focused={false}
                  kindLabel={
                    sessions.find((s) => s.id === sideTab.id)?.kind ?? "build"
                  }
                  showClose
                  onClosePane={() => setSideSessionId(null)}
                  onFocus={() => {
                    // Swap: side becomes primary, old primary becomes side
                    const prev = activeSessionId;
                    setActiveSessionId(sideTab.id);
                    if (prev && prev !== sideTab.id) setSideSessionId(prev);
                  }}
                />
              );
            })()}
        </div>

        {(layoutDensity === "wide" || layoutDensity === "ultrawide") &&
          tabs.length > 0 && (
            <FleetStrip
              tabs={tabs}
              activeSessionId={activeSessionId}
              sideSessionId={sideSessionId}
              canSplit={splitOk}
              onFocus={(id) => setActiveSessionId(id)}
              onOpenBeside={(id) => void openBeside(id)}
            />
          )}

        {showTerm && (
          <div className="terminal-slot">
            <TerminalPane toolShell={toolShell} />
          </div>
        )}

        <div className="composer-wrap">
          {slashOpen && slashHits.length > 0 && (
            <div className="slash-menu">
              {slashHits.map((c) => (
                <button
                  key={c.cmd}
                  type="button"
                  className="slash-item"
                  onClick={() => setComposer(c.cmd + " ")}
                >
                  <strong>{c.cmd}</strong>
                  <span className="slash-desc">{c.desc}</span>
                </button>
              ))}
            </div>
          )}
          <div className={`composer-shell ${busy ? "is-busy" : ""}`}>
            <textarea
              className="composer-input"
              value={composer}
              rows={2}
              placeholder={
                busy
                  ? "This session is running… switch tabs to start another"
                  : workspaceMode === "chat"
                    ? "Message Grok…"
                    : "Message the coding agent…"
              }
              onChange={(e) => setComposer(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  if (!busy) void sendPrompt();
                }
              }}
            />
            <div className="composer-toolbar">
              <div className="composer-toolbar-left">
                <label
                  className="composer-pill"
                  title="Model (default from Settings)"
                >
                  <span className="composer-pill-label">Model</span>
                  <select
                    value={
                      models.some((m) => m.id === status?.model)
                        ? (status?.model ?? models[0]?.id ?? "grok-build")
                        : (models[0]?.id ?? status?.model ?? "grok-build")
                    }
                    onChange={async (e) => {
                      await api.setModel(e.target.value);
                      await refreshChrome();
                    }}
                  >
                    {models.map((m) => (
                      <option key={m.id} value={m.id}>
                        {m.display_name}
                      </option>
                    ))}
                  </select>
                </label>
                <label
                  className="composer-pill"
                  title="Effort (default from Settings)"
                >
                  <span className="composer-pill-label">Effort</span>
                  <select
                    value={String(status?.effort ?? "medium")}
                    onChange={async (e) => {
                      await api.setEffort(e.target.value);
                      await refreshChrome();
                    }}
                  >
                    {[
                      "none",
                      "minimal",
                      "low",
                      "medium",
                      "high",
                      "xhigh",
                      "max",
                    ].map((e) => (
                      <option key={e} value={e}>
                        {e}
                      </option>
                    ))}
                  </select>
                </label>
                <button
                  type="button"
                  className={`composer-chip ${status?.always_approve ? "on" : ""}`}
                  title="Always approve tools (also in Settings)"
                  onClick={async () => {
                    await api.setAlwaysApprove(!status?.always_approve);
                    await refreshChrome();
                  }}
                >
                  Auto
                </button>
                <button
                  type="button"
                  className="composer-chip quiet"
                  title="Open settings"
                  onClick={() => setSettingsOpen(true)}
                >
                  ⚙
                </button>
                <button
                  type="button"
                  className={`composer-chip ${showTerm ? "on" : ""}`}
                  title={showTerm ? "Hide terminal" : "Show terminal"}
                  onClick={() => setShowTerm((v) => !v)}
                >
                  Term
                </button>
                {anyBusy && !busy && (
                  <span
                    className="composer-hint"
                    title="Other session tabs are still running"
                  >
                    {tabs.filter((t) => t.busy).length} other running
                  </span>
                )}
              </div>
              <div className="composer-toolbar-right">
                {busy && (
                  <button
                    type="button"
                    className="composer-stop"
                    title="Stop this session"
                    onClick={() => void api.sessionCancel(activeSessionId)}
                  >
                    Stop
                  </button>
                )}
                <button
                  type="button"
                  className="composer-send"
                  disabled={busy || !composer.trim()}
                  title={busy ? "Session busy" : "Send (Enter)"}
                  onClick={() => void sendPrompt()}
                >
                  <svg
                    width="16"
                    height="16"
                    viewBox="0 0 16 16"
                    fill="none"
                    aria-hidden
                  >
                    <path
                      d="M8 13V3M8 3L3.5 7.5M8 3l4.5 4.5"
                      stroke="currentColor"
                      strokeWidth="1.75"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    />
                  </svg>
                </button>
              </div>
            </div>
          </div>
        </div>
      </main>

      <aside className="rightbar">
        <div className="tabs">
          {(
            [
              "files",
              "git",
              "mcp",
              "plugins",
              "skills",
              "tasks",
              "rules",
            ] as RightTab[]
          ).map((t) => (
            <button
              key={t}
              type="button"
              className={`nav-tab ${rightTab === t ? "active" : ""}`}
              onClick={() => void loadRight(t)}
            >
              {t}
            </button>
          ))}
        </div>

        {rightTab === "files" && (
          <>
            <input
              placeholder="Fuzzy open…"
              value={fuzzy}
              onChange={(e) => void onFuzzy(e.target.value)}
              style={{
                width: "100%",
                marginBottom: 8,
                background: "var(--bg)",
                border: "1px solid var(--border)",
                borderRadius: 8,
                padding: "0.4rem",
              }}
            />
            {(fuzzyHits.length ? fuzzyHits : files).map((f) => (
              <button
                key={f}
                type="button"
                className="file-item"
                onClick={() => void sendPrompt(`read ${f}`)}
              >
                {f}
              </button>
            ))}
          </>
        )}

        {rightTab === "git" && (
          <>
            <div className="panel-block">
              <strong>Status</strong>
              <pre>{gitStatus || "(empty)"}</pre>
            </div>
            <div className="panel-block">
              <strong>Diff</strong>
              <pre>{gitDiff || "(no diff)"}</pre>
            </div>
            <button
              type="button"
              onClick={async () => setGitDiff(await api.agentEditDiffs())}
            >
              Agent edit diffs
            </button>
            <div className="panel-block">
              <strong>Worktrees</strong>
              <pre>{worktrees || "(none)"}</pre>
            </div>
            <button type="button" onClick={() => void api.gitStageAll()}>
              Stage all
            </button>
            <button
              type="button"
              onClick={() => void api.gitCommit("chore: GrokPtah commit")}
            >
              Commit
            </button>
          </>
        )}

        {rightTab === "mcp" && (
          <>
            {mcp.map((s) => (
              <div key={s.name} className="panel-block">
                <strong>{s.name}</strong> [{s.transport}] {s.status}
                <div>
                  <button
                    type="button"
                    onClick={async () => {
                      await api.mcpSetEnabled(s.name, !s.enabled);
                      setMcp(await api.mcpList());
                    }}
                  >
                    {s.enabled ? "Disable" : "Enable"}
                  </button>
                </div>
              </div>
            ))}
            <div className="panel-block">
              <strong>Doctor</strong>
              <pre>{mcpDoctor.join("\n")}</pre>
            </div>
            <button
              type="button"
              onClick={async () => {
                await api.mcpAddStdio("echo-tool", "echo", ["mcp-ok"]);
                setMcp(await api.mcpList());
                setMcpDoctor(await api.mcpDoctor());
              }}
            >
              Add sample stdio MCP
            </button>
          </>
        )}

        {rightTab === "plugins" &&
          plugins.map((p) => (
            <div key={p.id} className="panel-block">
              {p.name} {p.installed ? "✓" : ""}
              {!p.installed && (
                <button
                  type="button"
                  onClick={async () => {
                    await api.pluginInstall(p.id);
                    setPlugins(await api.pluginsList());
                  }}
                >
                  Install
                </button>
              )}
            </div>
          ))}

        {rightTab === "skills" && (
          <>
            {skills.map((s) => (
              <div key={s.id} className="panel-block">
                <strong>{s.name}</strong>
                <div style={{ color: "var(--muted)" }}>{s.description}</div>
              </div>
            ))}
            <div className="panel-block">
              <strong>Hooks config</strong>
              <pre>{hooksPreview ?? "Click Load hooks"}</pre>
              <button
                type="button"
                onClick={async () => {
                  setHooksPreview(await api.hooksConfig());
                }}
              >
                Load hooks
              </button>
            </div>
          </>
        )}

        {rightTab === "tasks" && (
          <>
            <div className="section-title">Subagents</div>
            {subagents.map((a) => (
              <div key={a.id} className="panel-block">
                {a.kind}: {a.title} — {a.status}
              </div>
            ))}
            <div className="section-title">Background / scheduled</div>
            {bgTasks.map((t) => (
              <div key={t.id} className="panel-block">
                {t.title} — {t.status}
                {t.status !== "cancelled" && (
                  <button
                    type="button"
                    onClick={async () => {
                      await api.cancelBackgroundTask(t.id);
                      setBgTasks(await api.backgroundTasks());
                    }}
                  >
                    Cancel
                  </button>
                )}
              </div>
            ))}
            <button
              type="button"
              onClick={async () => {
                await api.scheduleBackgroundTask("Manual schedule");
                setBgTasks(await api.backgroundTasks());
              }}
            >
              Schedule task
            </button>
          </>
        )}

        {rightTab === "rules" && (
          <div className="panel-block">
            <strong>Project rules</strong>
            <ul>
              {rules.map((r) => (
                <li key={r}>{r}</li>
              ))}
            </ul>
            {rules.length === 0 && <span style={{ color: "var(--muted)" }}>(none found)</span>}
          </div>
        )}

        {plan && (
          <div className="panel-block plan">
            <strong>Active plan</strong>
            <ol>
              {plan.steps.map((s, i) => (
                <li key={i}>{s}</li>
              ))}
            </ol>
          </div>
        )}
      </aside>

      <footer className="status-bar">
        <span className={busy || anyBusy ? "status-live" : "status-idle"}>
          {busy
            ? `● Live · ${activity.label}${activity.detail ? ` — ${activity.detail}` : ""}`
            : anyBusy
              ? `○ Idle here · ${tabs.filter((t) => t.busy).length} other tab(s) active`
              : "○ Idle · ready"}
          {" · "}
          {status?.running ? "agent up" : "agent stopped"} ·{" "}
          {status?.sandbox_profile}
        </span>
        <span>
          auto-update: {product.autoUpdateEnabled ? "on" : "off"} · bridge{" "}
          {product.bridgeVersion}
        </span>
      </footer>

      {permission && (
        <div className="modal-backdrop">
          <div className="modal permission-modal">
            <h3>Needs your response</h3>
            <p>{permission.summary}</p>
            <p style={{ fontSize: 12, color: "var(--muted)", marginTop: 0 }}>
              Tool: <code>{permission.tool_name}</code>
            </p>
            <details style={{ marginBottom: "0.75rem" }}>
              <summary style={{ cursor: "pointer", color: "var(--muted)", fontSize: 12 }}>
                Technical details
              </summary>
              <pre style={{ fontSize: 11, color: "var(--muted)", maxHeight: 160, overflow: "auto" }}>
                {JSON.stringify(permission.detail, null, 2)}
              </pre>
            </details>
            <div className="modal-actions">
              <button
                type="button"
                className="danger"
                onClick={async () => {
                  await api.permissionRespond(permission.id, "deny");
                  setPermission(null);
                  if (activeSessionId) {
                    patchTab(activeSessionId, (t) => ({
                      ...t,
                      needsPermission: false,
                      activity: {
                        ...t.activity,
                        phase: "tool",
                        label: "Working",
                        detail: "Permission denied",
                        live: true,
                        lastEventAt: Date.now(),
                      },
                    }));
                  }
                }}
              >
                Deny
              </button>
              <button
                type="button"
                onClick={async () => {
                  await api.permissionRespond(permission.id, "always_allow");
                  setPermission(null);
                  if (activeSessionId) {
                    patchTab(activeSessionId, (t) => ({
                      ...t,
                      needsPermission: false,
                      activity: {
                        ...t.activity,
                        phase: "tool",
                        label: "Working",
                        detail: "Continuing…",
                        live: true,
                        lastEventAt: Date.now(),
                      },
                    }));
                  }
                  await refreshChrome();
                }}
              >
                Always
              </button>
              <button
                type="button"
                className="primary"
                onClick={async () => {
                  await api.permissionRespond(permission.id, "allow");
                  setPermission(null);
                  if (activeSessionId) {
                    patchTab(activeSessionId, (t) => ({
                      ...t,
                      needsPermission: false,
                      activity: {
                        ...t.activity,
                        phase: "tool",
                        label: "Working",
                        detail: "Continuing…",
                        live: true,
                        lastEventAt: Date.now(),
                      },
                    }));
                  }
                }}
              >
                Allow
              </button>
            </div>
          </div>
        </div>
      )}

      <ContextMenu menu={ctxMenu} onClose={() => setCtxMenu(null)} />

      <SettingsPanel
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        models={models}
        auth={auth}
        onAuthChange={setAuth}
        onChromeChange={() => void refreshChrome()}
      />

      <SessionBrowser
        open={sessionBrowserOpen}
        activeSessionId={activeSessionId}
        onClose={() => setSessionBrowserOpen(false)}
        onOpen={(s) => void handleSessionBrowserOpen(s)}
        onChanged={() => void handleSessionBrowserChanged()}
      />

      <SearchPanel
        open={searchOpen}
        defaultKind={workspaceMode === "chat" ? "chat" : "build"}
        onClose={() => setSearchOpen(false)}
        onOpenSession={(sessionId, kind) => {
          void (async () => {
            try {
              if (kind === "chat" || kind === "build") {
                setWorkspaceMode(kind);
              }
              const s = await api.sessionLoad(sessionId);
              await openTab(s, true);
              setSearchOpen(false);
              await refreshSessions();
            } catch (e) {
              console.warn(e);
            }
          })();
        }}
      />

      {aboutOpen && (
        <div className="modal-backdrop" onClick={() => setAboutOpen(false)}>
          <div
            className="modal about-modal"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="about-hero">
              <BrandMark size={32} />
              <h3>GrokPtah</h3>
            </div>
            <p className="about-body">
              Desktop shell for Grok Build–style coding agents. Same workflow as
              the console TUI (sessions, tools, permissions), in a native
              window. Upstream crates and CLI remain in this repo for merge and
              console use.
              <br />
              <br />
              Bridge {product.bridgeVersion} · Apache-2.0
              <br />
              Upstream: xai-org/grok-build
              <br />
              CLI auto-update:{" "}
              {product.autoUpdateEnabled ? "on" : "off (desktop)"}
            </p>
            <div className="modal-actions">
              <button type="button" onClick={() => setAboutOpen(false)}>
                Close
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function sessionIdOf(u: SessionUpdate): string | null {
  if ("session_id" in u && typeof u.session_id === "string") {
    return u.session_id;
  }
  return null;
}

function ensureTab(tabs: SessionTab[], id: string): SessionTab[] {
  if (tabs.some((t) => t.id === id)) return tabs;
  return [...tabs, emptyTab(id)];
}

function mapTranscript(
  tab: SessionTab,
  map: (items: TranscriptItem[]) => TranscriptItem[],
  extra?: Partial<SessionTab>,
): SessionTab {
  return { ...tab, ...extra, transcript: map(tab.transcript) };
}

function applyUpdate(
  u: SessionUpdate,
  setTabs: React.Dispatch<React.SetStateAction<SessionTab[]>>,
  setPermission: React.Dispatch<React.SetStateAction<PermissionRequest | null>>,
) {
  const sid = sessionIdOf(u);
  if (!sid && u.type !== "permission_required") return;

  const withTab = (
    id: string,
    fn: (tab: SessionTab) => SessionTab,
  ) => {
    setTabs((prev) => {
      const base = ensureTab(prev, id);
      return base.map((t) => (t.id === id ? fn(t) : t));
    });
  };

  switch (u.type) {
    case "agent_message_chunk":
      withTab(sid!, (tab) => {
        const next = mapTranscript(
          tab,
          (t) => {
            const last = t[t.length - 1];
            if (last?.kind === "assistant") {
              const merged = mergeAssistantChunk(last.text, u.text);
              if (merged === "skip") return t;
              const copy = t.slice(0, -1);
              copy.push({
                kind: "assistant",
                text: merged,
                streaming: true,
              });
              return copy;
            }
            return [
              ...t,
              {
                kind: "assistant",
                text: collapseRepeatedText(u.text),
                streaming: true,
              },
            ];
          },
          { busy: true },
        );
        return withActivity(next, {
          ...phaseFromStream(),
          detail: "Streaming reply…",
          live: true,
        });
      });
      break;
    case "agent_thought_chunk":
      withTab(sid!, (tab) => {
        const snippet = u.text.trim().slice(0, 72);
        const next = mapTranscript(
          tab,
          (t) => {
            const last = t[t.length - 1];
            // Host thoughts are whole lines (status crumbs), not token streams.
            // Append on a new line when the last bubble is already a thought so
            // we never glue "kind=build…" into itself across deliveries.
            if (last?.kind === "thought") {
              // Exact duplicate of a multi-listener race — drop it.
              if (last.text === u.text || last.text.endsWith(u.text)) {
                return t;
              }
              const copy = t.slice(0, -1);
              copy.push({
                kind: "thought",
                text: `${last.text}\n${u.text}`,
                streaming: true,
              });
              return copy;
            }
            return [
              ...t,
              { kind: "thought", text: u.text, streaming: true },
            ];
          },
          { busy: true },
        );
        return withActivity(next, {
          ...phaseFromThought(),
          detail: snippet || "Reasoning…",
          live: true,
        });
      });
      break;
    case "tool_call":
      withTab(sid!, (tab) => {
        const next = mapTranscript(
          tab,
          (t) => [
            ...t,
            {
              kind: "tool",
              callId: u.call_id,
              title: u.title,
              status: u.status,
            },
          ],
          { busy: true },
        );
        return withActivity(next, {
          ...phaseFromTool(u.title),
          live: true,
        });
      });
      break;
    case "tool_call_update":
      withTab(sid!, (tab) => {
        const next = mapTranscript(tab, (t) =>
          t.map((item) =>
            item.kind === "tool" && item.callId === u.call_id
              ? {
                  ...item,
                  status: u.status,
                  output: u.output ?? item.output,
                }
              : item,
          ),
        );
        const running = next.transcript.some(
          (item) =>
            item.kind === "tool" &&
            (item.status === "running" || item.status === "pending"),
        );
        const toolRow = next.transcript.find(
          (item): item is Extract<TranscriptItem, { kind: "tool" }> =>
            item.kind === "tool" && item.callId === u.call_id,
        );
        const toolTitle = toolRow?.title ?? "tool";
        if (running) {
          return withActivity(next, {
            ...phaseFromTool(toolTitle),
            live: true,
          });
        }
        // Tool finished but turn may continue — keep live if still busy.
        return withActivity(next, {
          phase: next.busy ? "streaming" : next.activity.phase,
          label: next.busy ? "Working" : next.activity.label,
          detail: `${toolTitle} · ${u.status}`,
          live: next.busy,
        });
      });
      break;
    case "plan":
      withTab(sid!, (tab) =>
        mapTranscript(
          tab,
          (t) => [
            ...t,
            { kind: "plan", steps: u.steps, status: u.status },
          ],
          { plan: { steps: u.steps, status: u.status } },
        ),
      );
      break;
    case "permission_required":
      setPermission(u.request);
      if (sid) {
        withTab(sid, (tab) =>
          withActivity(
            { ...tab, busy: true, needsPermission: true, unseen: true },
            { ...phaseFromPermission(), live: true },
          ),
        );
      }
      break;
    case "shell_session_started":
      withTab(sid!, (tab) =>
        withActivity(
          { ...tab, busy: true },
          {
            phase: "tool",
            label: "Shell",
            detail: u.command?.slice(0, 80) ?? "running command",
            live: true,
          },
        ),
      );
      break;
    case "shell_output":
      withTab(sid!, (tab) =>
        withActivity(
          { ...tab, busy: true },
          {
            phase: "tool",
            label: "Shell",
            detail: "Streaming command output…",
            live: true,
          },
        ),
      );
      break;
    case "turn_complete":
      withTab(sid!, (tab) => ({
        ...tab,
        busy: false,
        activity: doneActivity(!!u.cancelled),
        transcript: tab.transcript.map((item) =>
          item.kind === "assistant" || item.kind === "thought"
            ? { ...item, streaming: false }
            : item,
        ),
      }));
      break;
    case "error":
      withTab(sid!, (tab) => {
        const next = mapTranscript(
          tab,
          (t) => [...t, { kind: "error", text: u.message }],
          { busy: false },
        );
        return {
          ...next,
          activity: errorActivity(u.message),
        };
      });
      break;
    default:
      break;
  }
}
