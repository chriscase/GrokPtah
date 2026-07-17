import { useCallback, useEffect, useMemo, useRef, useState } from "react";
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
import { PermissionModal } from "./components/PermissionModal";
import { StyledSelect } from "./components/StyledSelect";
import { LaunchSplash } from "./components/LaunchSplash";
import {
  applyAppearanceChrome,
  loadAppearanceChrome,
} from "./lib/appearance";
import {
  dequeuePermission,
  enqueuePermission,
  headPermission,
} from "./lib/permissionQueue";
import {
  clampDocks,
  SPLIT_MIN_WIDTH,
  useLayoutDensity,
  useMaxDocks,
} from "./lib/layout";
import {
  collapseRepeatedText,
  mergeAssistantChunk,
  subscribeSessionUpdates,
} from "./lib/sessionEvents";
import { displaySessionTitle } from "./lib/sessionTitle";
import { entriesToTranscriptItems } from "./lib/transcript";
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

  // Keep tools / plans / thoughts / errors — only collapse duplicate assistants.
  // Tool cards must survive turn finalize or the transcript looks "empty" of work.
  const nonAssistant: TranscriptItem[] = collapseAdjacentDuplicateAssistants(
    tail,
  )
    .filter((item) => item.kind !== "assistant")
    .map((item) => {
      if (item.kind === "thought") {
        return { kind: "thought" as const, text: item.text, streaming: false };
      }
      if (item.kind === "tool") {
        return {
          ...item,
          status: typeof item.status === "string" ? item.status : "completed",
        };
      }
      return item;
    });

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
  /**
   * Per-session composer drafts — survive focus switches so you can
   * pre-type prompts for several agents and send when ready.
   */
  const [composerDrafts, setComposerDrafts] = useState<Record<string, string>>(
    {},
  );
  /** Taller composer for detailed prompts (power users). */
  const [composerExpanded, setComposerExpanded] = useState(false);
  /** Per-session prompt queue while a turn is running (#147). */
  const [promptQueues, setPromptQueues] = useState<Record<string, string[]>>(
    {},
  );
  const [ctxMenu, setCtxMenu] = useState<ContextMenuState | null>(null);
  /** FIFO of tool permission prompts — concurrent requests must not clobber (#141). */
  const [permissionQueue, setPermissionQueue] = useState<PermissionRequest[]>(
    [],
  );
  const permission = headPermission(permissionQueue);
  const [rightTab, setRightTab] = useState<RightTab>("files");
  const [files, setFiles] = useState<string[]>([]);
  const [fuzzy, setFuzzy] = useState("");
  const [fuzzyHits, setFuzzyHits] = useState<string[]>([]);
  const [gitStatus, setGitStatus] = useState("");
  const [gitDiff, setGitDiff] = useState("");
  const [worktrees, setWorktrees] = useState("");
  const [wtPath, setWtPath] = useState("../wt-feature");
  const [wtBranch, setWtBranch] = useState("");
  const [wtBusy, setWtBusy] = useState(false);
  const [mcp, setMcp] = useState<any[]>([]);
  const [mcpDoctor, setMcpDoctor] = useState<string[]>([]);
  /** Project-local .mcp.json trust prompt (malicious-repo RCE gate). */
  const [mcpTrustPrompt, setMcpTrustPrompt] = useState<{
    project: string | null;
    has_local_mcp: boolean;
    trusted: boolean;
    decided: boolean;
  } | null>(null);
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
  /** Terminal panel expanded (collapsed strip by default — space efficient). */
  const [showTerm, setShowTerm] = useState(false);
  /**
   * Once the user opens the terminal, keep TerminalPane mounted across collapse
   * so the PTY + xterm survive (#136). Hidden with CSS when `showTerm` is false.
   */
  const [termEverOpened, setTermEverOpened] = useState(false);
  /** Thin bar when a tool shell is live but panel is collapsed. */
  const [termPeek, setTermPeek] = useState(false);
  const [toolShell, setToolShell] = useState<ToolShellAttach | null>(null);
  const [aboutOpen, setAboutOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  /** False until we finish reopening tabs from ~/.grokptah/workspace.json. */
  const [workspaceRestored, setWorkspaceRestored] = useState(false);
  const [sessionBrowserOpen, setSessionBrowserOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  /**
   * Ordered dock slots (session ids visible as columns). Phase 14.1 multi-zone.
   * Empty or single-element = classic single-pane; up to maxDocks columns.
   */
  const [docks, setDocks] = useState<string[]>([]);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [rightbarCollapsed, setRightbarCollapsed] = useState(false);
  const [liveHidden, setLiveHidden] = useState(false);
  const stageRef = useRef<HTMLDivElement | null>(null);
  const layoutDensity = useLayoutDensity();
  const maxDocks = useMaxDocks(stageRef, layoutDensity);
  const splitOk = maxDocks >= 2;
  const [workspaceMode, setWorkspaceMode] = useState<WorkspaceMode>("build");

  // Keep docks valid: unique, open tabs only, within capacity, include focus
  useEffect(() => {
    setDocks((prev) => {
      const open = new Set(tabs.map((t) => t.id));
      let next = prev.filter((id) => open.has(id));
      if (activeSessionId && open.has(activeSessionId) && !next.includes(activeSessionId)) {
        next = [activeSessionId, ...next];
      }
      if (next.length === 0 && activeSessionId && open.has(activeSessionId)) {
        next = [activeSessionId];
      }
      next = clampDocks(next, Math.max(1, maxDocks), activeSessionId);
      // Avoid useless re-renders
      if (
        next.length === prev.length &&
        next.every((id, i) => id === prev[i])
      ) {
        return prev;
      }
      return next;
    });
  }, [tabs, activeSessionId, maxDocks]);

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
        // Resume: promote backend active session + cwd, then hydrate transcript (#38).
        const loaded = await api.sessionLoad(summary.id);
        const entries = await api.sessionTranscript(loaded.id);
        setTabs((prev) =>
          prev.map((t) => {
            if (t.id !== loaded.id) return t;
            // Keep live stream if this tab already has more than disk.
            if (t.busy && t.transcript.length > entries.length) return t;
            return {
              ...t,
              title: loaded.title || summary.title,
              transcript: entriesToTranscriptItems(entries),
            };
          }),
        );
        setStatus((st) =>
          st
            ? {
                ...st,
                active_session: loaded.id,
                project_cwd: loaded.cwd || st.project_cwd,
              }
            : st,
        );
        // #152: show historical subagent summary after reopen (host loads from disk).
        setSubagents(await api.subagentsList());
      } catch {
        /* offline / empty */
      }
    },
    [],
  );

  const closeTab = useCallback((id: string) => {
    setDocks((d) => d.filter((x) => x !== id));
    setComposerDrafts((d) => {
      if (!(id in d)) return d;
      const next = { ...d };
      delete next[id];
      return next;
    });
    setTabs((prev) => {
      const next = prev.filter((t) => t.id !== id);
      setActiveSessionId((cur) => {
        if (cur !== id) return cur;
        return next[next.length - 1]?.id ?? null;
      });
      return next;
    });
  }, []);

  /** Write composer into the focused session's draft bag. */
  const updateComposer = useCallback(
    (text: string) => {
      setComposer(text);
      if (activeSessionId) {
        setComposerDrafts((d) => {
          if ((d[activeSessionId] ?? "") === text) return d;
          if (!text) {
            if (!(activeSessionId in d)) return d;
            const next = { ...d };
            delete next[activeSessionId];
            return next;
          }
          return { ...d, [activeSessionId]: text };
        });
      }
    },
    [activeSessionId],
  );

  // Restore draft when focus moves between sessions
  useEffect(() => {
    if (!activeSessionId) {
      setComposer("");
      return;
    }
    setComposer(composerDrafts[activeSessionId] ?? "");
    // Only rehydrate when the target session changes — not on every draft keystroke.
    // eslint-disable-next-line react-hooks/exhaustive-deps -- intentional: load on focus switch only
  }, [activeSessionId]);

  const undockSession = useCallback((id: string) => {
    setDocks((d) => {
      if (d.length <= 1) return d;
      return d.filter((x) => x !== id);
    });
  }, []);

  const focusSession = useCallback((id: string) => {
    setActiveSessionId(id);
  }, []);

  const hideLiveRail = useCallback(() => {
    setLiveHidden(true);
  }, []);


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
      // Apply persisted appearance so Light is real after reload (#133).
      document.documentElement.dataset.theme =
        st.appearance === "light" ? "light" : "dark";
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
    applyAppearanceChrome(loadAppearanceChrome());
    void refreshChrome();
  }, [refreshChrome]);

  // Page-lifetime singleton bus (survives StrictMode + Vite HMR). Raw
  // listen() per mount stacked handlers and glued the same reply N times.
  useEffect(() => {
    return subscribeSessionUpdates((u) => {
      if (u.type === "shell_session_started") {
        // Attach tool shell but keep the panel collapsed unless already open.
        setToolShell({ callId: u.call_id, command: u.command });
        setTermPeek(true);
        // #52: agent shells appear in Tasks without waiting for turn end.
        void api.backgroundTasks().then(setBgTasks).catch(() => {});
      }
      if (u.type === "shell_session_ended") {
        // Keep peek until user dismisses or opens term
        setTermPeek(true);
        void api.backgroundTasks().then(setBgTasks).catch(() => {});
      }
      if (u.type === "background_task") {
        void api.backgroundTasks().then(setBgTasks).catch(() => {});
      }
      if (u.type === "subagent_spawned" || u.type === "subagent_update") {
        void api.subagentsList().then(setSubagents).catch(() => {});
      }
      if (u.type === "file_edit") {
        // Live agent diffs in the git pane (no manual refresh).
        setRightTab("git");
        setGitDiff((prev) => {
          const header = `--- ${u.path} ---`;
          const block = `${header}\n${u.unified_diff || u.summary}\n`;
          if (!prev.trim()) return block;
          const re = new RegExp(
            `--- ${u.path.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")} ---\\n[\\s\\S]*?(?=--- |$)`,
          );
          if (re.test(prev)) {
            return prev.replace(re, block);
          }
          return `${prev.trimEnd()}\n${block}`;
        });
      }
      applyUpdate(u, setTabs, setPermissionQueue);
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
          if (!cancelled) await maybePromptMcpTrust();
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

  // Persist tab *ids* only (not per-token transcript rewrites).
  // Depending on full `tabs` re-wrote workspace.json on every stream chunk.
  const openTabIdsKey = tabs.map((t) => t.id).join(",");
  useEffect(() => {
    if (!workspaceRestored) return;
    void api.setOpenTabs(
      openTabIdsKey ? openTabIdsKey.split(",") : [],
      activeSessionId,
    );
  }, [openTabIdsKey, activeSessionId, workspaceRestored]);

  const slashOpen = composer.startsWith("/") && !composer.includes(" ");
  const slashHits = useMemo(
    () =>
      SLASH_COMMANDS.filter((c) =>
        c.cmd.startsWith(composer || "/"),
      ),
    [composer],
  );

  async function maybePromptMcpTrust() {
    try {
      const trust = await api.mcpProjectTrust();
      // Prompt only when local config exists and user has never answered.
      if (trust.has_local_mcp && !trust.decided) {
        setMcpTrustPrompt(trust);
      } else {
        setMcpTrustPrompt(null);
      }
    } catch {
      setMcpTrustPrompt(null);
    }
  }

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
      await maybePromptMcpTrust();
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
        await maybePromptMcpTrust();
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

  /**
   * Dock a session into the next free column (or create one).
   * When already at capacity with multiple docks, peels the rightmost
   * unfocused dock (toggle-down). When at max with one request to grow,
   * replaces the rightmost unfocused dock.
   */
  const openBeside = useCallback(
    async (sessionId?: string) => {
      if (maxDocks < 2) return;
      const primary = activeSessionId;

      // Toggle peel: multi-dock + no specific target → remove rightmost unfocused
      if (!sessionId && docks.length > 1) {
        setDocks((d) => {
          if (d.length <= 1) return d;
          const peel =
            [...d].reverse().find((id) => id !== primary) ?? d[d.length - 1];
          return d.filter((id) => id !== peel);
        });
        return;
      }

      let targetId = sessionId;
      if (!targetId || targetId === primary) {
        const other = tabs.find(
          (t) => t.id !== primary && !docks.includes(t.id),
        );
        if (other) {
          targetId = other.id;
        } else if (!targetId || targetId === primary) {
          const undocked = tabs.find((t) => t.id !== primary);
          if (undocked && !docks.includes(undocked.id)) {
            targetId = undocked.id;
          } else {
            const s = await createSession(workspaceMode);
            if (!s) return;
            if (primary) setActiveSessionId(primary);
            targetId = s.id;
          }
        }
      }

      if (!targetId) return;

      // Ensure tab open
      if (!tabs.some((t) => t.id === targetId)) {
        let summary = sessions.find((s) => s.id === targetId);
        if (!summary) {
          try {
            summary = await api.sessionLoad(targetId);
          } catch {
            return;
          }
        }
        const keepFocus = primary;
        await openTab(summary, true);
        if (keepFocus) setActiveSessionId(keepFocus);
      }

      setDocks((d) => {
        let next = d.filter((id) => id !== targetId);
        if (primary && !next.includes(primary)) {
          next = [primary, ...next];
        }
        if (next.includes(targetId!)) return clampDocks(next, maxDocks, primary);
        if (next.length < maxDocks) {
          next = [...next, targetId!];
        } else {
          // Replace rightmost unfocused
          const replaceAt = (() => {
            for (let i = next.length - 1; i >= 0; i--) {
              if (next[i] !== primary) return i;
            }
            return next.length - 1;
          })();
          next = next.slice();
          next[replaceAt] = targetId!;
        }
        return clampDocks(next, maxDocks, primary);
      });
    },
    [
      maxDocks,
      activeSessionId,
      docks,
      tabs,
      sessions,
      openTab,
      createSession,
      workspaceMode,
    ],
  );

  // Keyboard: multi-zone + chrome (capture so composer/webview don't eat them)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const meta = e.metaKey || e.ctrlKey;
      if (!meta) return;

      // ⌘B left chrome, ⌘⌥B right chrome, ⌘⇧L Live
      const isB = e.key === "b" || e.key === "B" || e.code === "KeyB";
      if (isB) {
        if (e.altKey) {
          e.preventDefault();
          e.stopPropagation();
          setRightbarCollapsed((v) => !v);
          return;
        }
        if (!e.shiftKey) {
          e.preventDefault();
          e.stopPropagation();
          setSidebarCollapsed((v) => !v);
          return;
        }
      }
      if (e.shiftKey && (e.key === "l" || e.key === "L" || e.code === "KeyL")) {
        e.preventDefault();
        e.stopPropagation();
        setLiveHidden((v) => !v);
        return;
      }

      // ⌘1–⌘6 focus dock by zone index
      if (e.key >= "1" && e.key <= "6" && !e.altKey && !e.shiftKey) {
        const idx = Number(e.key) - 1;
        if (docks[idx]) {
          e.preventDefault();
          setActiveSessionId(docks[idx]);
        }
        return;
      }

      // ⌘⌥← / ⌘⌥→ cycle docks
      if (e.altKey && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
        if (docks.length < 2 || !activeSessionId) return;
        e.preventDefault();
        const i = docks.indexOf(activeSessionId);
        if (i < 0) return;
        const next =
          e.key === "ArrowLeft"
            ? docks[(i - 1 + docks.length) % docks.length]
            : docks[(i + 1) % docks.length];
        setActiveSessionId(next);
        return;
      }

      // ⌘\ toggle multi-dock
      if (e.key === "\\") {
        e.preventDefault();
        if (maxDocks < 2) return;
        void openBeside();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [docks, activeSessionId, maxDocks, openBeside]);

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
            label: splitOk
              ? "Open beside"
              : `Open beside (widen to ≥${SPLIT_MIN_WIDTH}px)`,
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
            id: "resume",
            label: "Resume (load history)",
            onClick: () => {
              void (async () => {
                try {
                  const s = await api.sessionLoad(sessionId);
                  await openTab(s, true);
                  setSessions(
                    await api.sessionListByKind(workspaceMode, false),
                  );
                } catch (e) {
                  console.warn(e);
                }
              })();
            },
          },
          {
            type: "item",
            id: "fork",
            label: "Fork",
            onClick: () => {
              void (async () => {
                const f = await api.sessionFork(sessionId);
                // Fork: open new id with history copy from source tab + disk.
                await openTab(f, true);
                const src = tabs.find((t) => t.id === sessionId);
                if (src && src.transcript.length > 0) {
                  patchTab(f.id, (t) => ({
                    ...t,
                    transcript:
                      t.transcript.length >= src.transcript.length
                        ? t.transcript
                        : [...src.transcript],
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
            id: "rewind-conv",
            label: "Rewind chat only",
            onClick: () => {
              void (async () => {
                const tab = tabs.find((t) => t.id === sessionId);
                const keep = Math.max(0, (tab?.transcript.length ?? 1) - 1);
                await api.sessionRewind(sessionId, keep, "conversation");
                const list = await api.sessionListByKind(workspaceMode, false);
                setSessions(list);
                const summary = list.find((s) => s.id === sessionId);
                if (summary) await openTab(summary, true);
              })();
            },
          },
          {
            type: "item",
            id: "rewind-files",
            label: "Rewind files only (agent edits)",
            onClick: () => {
              void (async () => {
                const tab = tabs.find((t) => t.id === sessionId);
                const keep = tab?.transcript.length ?? 0;
                await api.sessionRewind(sessionId, keep, "files");
                setGitDiff("(files restored from pre-edit snapshots)");
                setRightTab("git");
              })();
            },
          },
          {
            type: "item",
            id: "rewind-all",
            label: "Rewind chat + files",
            onClick: () => {
              void (async () => {
                const tab = tabs.find((t) => t.id === sessionId);
                const keep = Math.max(0, (tab?.transcript.length ?? 1) - 1);
                await api.sessionRewind(sessionId, keep, "all");
                const list = await api.sessionListByKind(workspaceMode, false);
                setSessions(list);
                const summary = list.find((s) => s.id === sessionId);
                if (summary) await openTab(summary, true);
                setGitDiff("(chat + files rewound)");
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
                    transcript: entriesToTranscriptItems(entries),
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

  async function sendPrompt(
    text?: string,
    opts?: { interject?: boolean; fromQueue?: boolean; sessionId?: string },
  ) {
    const prompt = (text ?? composer).trim();
    if (!prompt) return;
    const fromComposer = text === undefined;
    if (fromComposer) {
      setComposer("");
      if (activeSessionId) {
        setComposerDrafts((d) => {
          if (!(activeSessionId in d)) return d;
          const next = { ...d };
          delete next[activeSessionId];
          return next;
        });
      }
    }
    let id: string;
    try {
      id = opts?.sessionId ?? (await ensureSession());
    } catch (e) {
      console.warn(e);
      // Restore draft if session creation failed after we cleared it
      if (fromComposer) updateComposer(prompt);
      return;
    }
    // After ensureSession, focus may have moved to a new id — clear that draft too
    if (fromComposer) {
      setComposer("");
      setComposerDrafts((d) => {
        if (!(id in d) && !(activeSessionId && activeSessionId in d)) return d;
        const next = { ...d };
        delete next[id];
        if (activeSessionId) delete next[activeSessionId];
        return next;
      });
    }

    const tabBusy = tabs.find((t) => t.id === id)?.busy;
    // #147: queue while a turn is running (unless interject = cancel then send).
    if (tabBusy && !opts?.interject && !opts?.fromQueue) {
      const nextLen = (promptQueues[id]?.length ?? 0) + 1;
      setPromptQueues((q) => ({
        ...q,
        [id]: [...(q[id] ?? []), prompt],
      }));
      patchTab(id, (t) => ({
        ...t,
        activity: {
          ...t.activity,
          detail: `Queued (${nextLen})`,
          lastEventAt: Date.now(),
        },
        transcript: [
          ...t.transcript,
          {
            kind: "thought" as const,
            text: `Queued while turn runs: ${prompt.slice(0, 120)}${prompt.length > 120 ? "…" : ""}`,
          },
        ],
      }));
      return;
    }
    if (tabBusy && opts?.interject) {
      try {
        await api.sessionCancel(id);
      } catch {
        /* best effort */
      }
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
            transcript: entriesToTranscriptItems(entries),
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
      // #148: fork / rename / export / cd
      if (prompt === "/fork" || prompt.startsWith("/fork ")) {
        try {
          const f = await api.sessionFork(id);
          await openTab(f, true);
          patchTab(id, (t) => ({
            ...t,
            busy: false,
            activity: idleActivity(),
            transcript: t.transcript.filter(
              (x) => !(x.kind === "user" && x.text === prompt),
            ),
          }));
          await refreshSessions();
        } catch (e) {
          patchTab(id, (t) => ({
            ...t,
            busy: false,
            activity: errorActivity(String(e)),
          }));
        }
        return;
      }
      if (prompt.startsWith("/rename ")) {
        const title = prompt.slice("/rename ".length).trim();
        try {
          if (!title) throw new Error("Usage: /rename <title>");
          await api.sessionRename(id, title);
          patchTab(id, (t) => ({
            ...t,
            title,
            busy: false,
            activity: idleActivity(),
            transcript: [
              ...t.transcript.filter(
                (x) => !(x.kind === "user" && x.text === prompt),
              ),
              { kind: "assistant", text: `Renamed session to “${title}”.` },
            ],
          }));
          await refreshSessions();
        } catch (e) {
          patchTab(id, (t) => ({
            ...t,
            busy: false,
            activity: errorActivity(String(e)),
          }));
        }
        return;
      }
      if (prompt === "/export" || prompt.startsWith("/export ")) {
        try {
          const text = await api.exportTranscript(id);
          await navigator.clipboard.writeText(text);
          patchTab(id, (t) => ({
            ...t,
            busy: false,
            activity: idleActivity(),
            transcript: [
              ...t.transcript.filter(
                (x) => !(x.kind === "user" && x.text === prompt),
              ),
              {
                kind: "assistant",
                text: `Exported transcript (${text.length} chars) to clipboard.`,
              },
            ],
          }));
        } catch (e) {
          patchTab(id, (t) => ({
            ...t,
            busy: false,
            activity: errorActivity(String(e)),
          }));
        }
        return;
      }
      if (prompt.startsWith("/cd ") || prompt === "/cd") {
        const path = prompt === "/cd" ? "" : prompt.slice("/cd ".length).trim();
        try {
          if (!path) throw new Error("Usage: /cd <path>");
          await api.setProjectCwd(path);
          try {
            await api.sessionSetCwd(id, path);
          } catch {
            /* session cwd optional */
          }
          await refreshChrome();
          patchTab(id, (t) => ({
            ...t,
            busy: false,
            activity: idleActivity(),
            transcript: [
              ...t.transcript.filter(
                (x) => !(x.kind === "user" && x.text === prompt),
              ),
              { kind: "assistant", text: `Working directory → ${path}` },
            ],
          }));
        } catch (e) {
          patchTab(id, (t) => ({
            ...t,
            busy: false,
            activity: errorActivity(String(e)),
          }));
        }
        return;
      }
      // /resume → session browser; /continue → most recently updated other session (#38).
      if (prompt === "/resume") {
        patchTab(id, (t) => ({
          ...t,
          busy: false,
          activity: idleActivity(),
          transcript: t.transcript.filter(
            (x) => !(x.kind === "user" && x.text === prompt),
          ),
        }));
        setSessionBrowserOpen(true);
        return;
      }
      if (prompt === "/continue") {
        try {
          const list = await api.sessionListByKind(workspaceMode, false);
          const sorted = [...list].sort(
            (a, b) =>
              new Date(b.updated_at).getTime() -
              new Date(a.updated_at).getTime(),
          );
          const target = sorted.find((s) => s.id !== id) ?? null;
          if (!target) {
            patchTab(id, (t) => ({
              ...t,
              busy: false,
              activity: idleActivity(),
              transcript: [
                ...t.transcript.filter(
                  (x) => !(x.kind === "user" && x.text === prompt),
                ),
                {
                  kind: "assistant",
                  text: "No other session to continue. Use /resume to browse history, or Fork from the tab menu.",
                },
              ],
            }));
            return;
          }
          // Drop the slash echo from the previous tab.
          patchTab(id, (t) => ({
            ...t,
            busy: false,
            activity: idleActivity(),
            transcript: t.transcript.filter(
              (x) => !(x.kind === "user" && x.text === prompt),
            ),
          }));
          await openTab(target, true);
          patchTab(target.id, (t) => ({
            ...t,
            busy: false,
            activity: idleActivity(),
            transcript: [
              ...t.transcript,
              {
                kind: "assistant",
                text: `Continued “${target.title || target.id.slice(0, 8)}”. Send a prompt to keep going.`,
              },
            ],
          }));
        } catch (e) {
          patchTab(id, (t) => ({
            ...t,
            busy: false,
            activity: errorActivity(String(e)),
          }));
        }
        return;
      }
      const reply = await api.sessionPrompt(id, prompt);
      // Prefer durable transcript (includes tool rows) over pure event state.
      // Events may lag or miss tool cards; disk is the source of truth after the turn.
      try {
        const entries = await api.sessionTranscript(id);
        if (entries.length > 0) {
          patchTab(id, (t) => ({
            ...t,
            busy: false,
            activity: doneActivity(false),
            transcript: entriesToTranscriptItems(entries),
          }));
        } else {
          patchTab(id, (t) => ({
            ...t,
            busy: false,
            activity: doneActivity(false),
            transcript: finalizeTurnTranscript(t.transcript, reply),
          }));
        }
      } catch {
        patchTab(id, (t) => ({
          ...t,
          busy: false,
          activity: doneActivity(false),
          transcript: finalizeTurnTranscript(t.transcript, reply),
        }));
      }
      setSubagents(await api.subagentsList());
      setBgTasks(await api.backgroundTasks());
      await refreshChrome();
      await refreshSessions();
      // #147: drain next queued prompt for this session.
      let nextQueued: string | undefined;
      setPromptQueues((q) => {
        const list = q[id] ?? [];
        if (!list.length) return q;
        const [head, ...rest] = list;
        nextQueued = head;
        const next = { ...q };
        if (rest.length) next[id] = rest;
        else delete next[id];
        return next;
      });
      if (nextQueued) {
        const drain = nextQueued;
        // fromQueue bypasses busy re-queue (state may still show busy briefly).
        setTimeout(() => {
          void sendPrompt(drain, { fromQueue: true, sessionId: id });
        }, 0);
      }
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

  const splashReady = workspaceRestored && status !== null;

  return (
    <div
      className={`app-shell ${sidebarCollapsed ? "sidebar-collapsed" : ""} ${rightbarCollapsed ? "rightbar-collapsed" : ""}`}
    >
      <LaunchSplash ready={splashReady} />
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
          <div className="chrome-toggles" role="group" aria-label="Layout panels">
            <button
              type="button"
              className={`chrome-toggle ${sidebarCollapsed ? "" : "is-on"}`}
              title={
                sidebarCollapsed
                  ? "Show sessions sidebar (⌘B)"
                  : "Hide sessions sidebar (⌘B)"
              }
              aria-pressed={!sidebarCollapsed}
              onClick={() => setSidebarCollapsed((v) => !v)}
            >
              <span className="chrome-toggle-icon" aria-hidden>
                {sidebarCollapsed ? "▏" : "◂"}
              </span>
              Sessions
            </button>
            <button
              type="button"
              className={`chrome-toggle ${rightbarCollapsed ? "" : "is-on"}`}
              title={
                rightbarCollapsed
                  ? "Show tools panel (⌘⌥B)"
                  : "Hide tools panel (⌘⌥B)"
              }
              aria-pressed={!rightbarCollapsed}
              onClick={() => setRightbarCollapsed((v) => !v)}
            >
              Tools
              <span className="chrome-toggle-icon" aria-hidden>
                {rightbarCollapsed ? "▕" : "▸"}
              </span>
            </button>
            <button
              type="button"
              className={`chrome-toggle ${liveHidden ? "" : "is-on"}`}
              title={
                liveHidden
                  ? "Show Live session rail (⌘⇧L)"
                  : "Hide Live session rail (⌘⇧L)"
              }
              aria-pressed={!liveHidden}
              onClick={() => setLiveHidden((v) => !v)}
            >
              Live
            </button>
          </div>
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

      <aside
        className={`sidebar ${sidebarCollapsed ? "is-collapsed" : ""}`}
        aria-expanded={!sidebarCollapsed}
      >
        <button
          type="button"
          className="rail-expand"
          title="Show sessions sidebar (⌘B)"
          aria-label="Show sessions sidebar"
          onClick={() => setSidebarCollapsed(false)}
        >
          <span className="rail-expand-chevron" aria-hidden>
            ▸
          </span>
          <span className="rail-expand-label">Sessions</span>
        </button>
        <div className="panel-body">
        <div className="panel-chrome">
          <span className="panel-chrome-title">Sessions</span>
          <button
            type="button"
            className="panel-collapse-btn"
            title="Hide sessions sidebar (⌘B)"
            aria-label="Hide sessions sidebar"
            onClick={() => setSidebarCollapsed(true)}
          >
            ◂
          </button>
        </div>
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
                    <span className="session-item-name" title={s.title}>
                      {displaySessionTitle(s, sessions)}
                    </span>
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
        </div>
      </aside>

      <main
        className={`main density-${layoutDensity} ${
          docks.length > 1 ? "is-split" : ""
        }`}
      >
        {tabs.length > 0 && (
          <div className="session-tabs" role="tablist" aria-label="Open sessions">
            {tabs.map((t) => (
              <div
                key={t.id}
                className={`session-tab ${t.id === activeSessionId ? "active" : ""} ${docks.includes(t.id) ? "is-docked" : ""} ${t.busy ? "busy" : ""} ${t.needsPermission ? "needs-permission" : ""} ${t.unseen ? "has-unseen" : ""}`}
                draggable
                onDragStart={(e) => {
                  e.dataTransfer.setData("text/session-id", t.id);
                  e.dataTransfer.effectAllowed = "move";
                }}
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
                  <span
                    className="session-tab-text"
                    title={t.title}
                  >
                    {displaySessionTitle(
                      {
                        id: t.id,
                        title: t.title,
                        cwd: sessions.find((s) => s.id === t.id)?.cwd,
                      },
                      [
                        ...sessions,
                        ...tabs.map((x) => ({
                          id: x.id,
                          title: x.title,
                          cwd: sessions.find((s) => s.id === x.id)?.cwd,
                        })),
                      ],
                    )}
                  </span>
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
            {tabs.length >= 1 && (
              <button
                type="button"
                className={`session-tab-split ${docks.length > 1 ? "on" : ""} ${!splitOk ? "is-disabled" : ""}`}
                disabled={!splitOk}
                title={
                  !splitOk
                    ? `Need more stage width for multi-dock (collapse rails with ⌘B / ⌘⌥B, or widen ≥${SPLIT_MIN_WIDTH}px)`
                    : docks.length > 1
                      ? `Add dock or peel last unfocused (⌘\\) · capacity ${docks.length}/${maxDocks}`
                      : `Open another session in a new column (⌘\\) · up to ${maxDocks} docks`
                }
                onClick={() => {
                  if (!splitOk) return;
                  void openBeside();
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
          ref={stageRef}
          className={`pane-row zones-${Math.max(1, docks.length)} ${
            docks.length > 1 ? "has-zones" : "single"
          }`}
        >
          {docks.length === 0 ? (
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
                    Up to 6 zones on ultrawide: ⧉ or ⌘\\ · collapse rails (⌘B) ·
                    Live rail to switch
                  </li>
                </ul>
              </div>
            </div>
          ) : (
            docks.map((dockId, zoneIndex) => {
              const dockTab = tabs.find((t) => t.id === dockId);
              if (!dockTab) return null;
              const isFocused = dockId === activeSessionId;
              return (
                <div
                  key={dockId}
                  className={`dock-slot ${isFocused ? "is-focused-dock" : ""}`}
                  data-zone={zoneIndex + 1}
                  onDragOver={(e) => {
                    e.preventDefault();
                    e.dataTransfer.dropEffect = "move";
                  }}
                  onDrop={(e) => {
                    e.preventDefault();
                    const sid = e.dataTransfer.getData("text/session-id");
                    if (!sid || sid === dockId) return;
                    setDocks((d) => {
                      const next = d.slice();
                      const from = next.indexOf(sid);
                      const to = next.indexOf(dockId);
                      if (to < 0) return d;
                      if (from >= 0) {
                        // swap
                        next[from] = dockId;
                        next[to] = sid;
                      } else {
                        next[to] = sid;
                      }
                      return clampDocks(next, maxDocks, activeSessionId);
                    });
                    setActiveSessionId(sid);
                  }}
                >
                  <SessionPane
                    tab={dockTab}
                    focused={isFocused}
                    zoneIndex={zoneIndex + 1}
                    zoneCount={docks.length}
                    kindLabel={
                      sessions.find((s) => s.id === dockTab.id)?.kind ??
                      workspaceMode
                    }
                    bridgeVersion={product.bridgeVersion}
                    emptyHint={
                      workspaceMode === "build"
                        ? "Set a working directory, then send a prompt."
                        : "Message Grok when this pane is focused."
                    }
                    showClose={docks.length > 1}
                    onClosePane={undockSession}
                    onFocusSession={focusSession}
                    cwd={sessions.find((s) => s.id === dockTab.id)?.cwd}
                    titlePeers={[
                      ...sessions,
                      ...tabs.map((x) => ({
                        id: x.id,
                        title: x.title,
                        cwd: sessions.find((s) => s.id === x.id)?.cwd,
                      })),
                    ]}
                  />
                </div>
              );
            })
          )}
        </div>

        {!liveHidden && tabs.length > 0 && (
            <FleetStrip
              tabs={tabs}
              activeSessionId={activeSessionId}
              zoneIds={docks}
              canSplit={splitOk}
              onFocus={focusSession}
              onOpenBeside={openBeside}
              onHide={hideLiveRail}
            />
          )}

        {/* Tool shell: compact peek by default; expand only when user wants it */}
        {!showTerm && (termPeek || toolShell) && (
          <div className="terminal-peek">
            <button
              type="button"
              className="terminal-peek-main"
              title="Expand tool shell"
              onClick={() => {
                setShowTerm(true);
                setTermEverOpened(true);
                setTermPeek(false);
              }}
            >
              <span className="terminal-peek-label">Shell</span>
              <span className="terminal-peek-cmd">
                {toolShell?.command
                  ? toolShell.command.length > 72
                    ? `${toolShell.command.slice(0, 72)}…`
                    : toolShell.command
                  : "Agent terminal available"}
              </span>
              <span className="terminal-peek-action">Expand</span>
            </button>
            <button
              type="button"
              className="terminal-peek-dismiss"
              title="Dismiss shell bar"
              onClick={() => {
                setTermPeek(false);
                setToolShell(null);
              }}
            >
              ×
            </button>
          </div>
        )}
        {/* #136: keep TerminalPane mounted after first open; CSS-hide on collapse. */}
        {(showTerm || termEverOpened) && (
          <div
            className={`terminal-slot ${showTerm ? "is-expanded" : "is-collapsed"}`}
            aria-hidden={!showTerm}
          >
            {showTerm && (
              <div className="terminal-slot-bar">
                <span className="terminal-slot-title">
                  {toolShell?.command
                    ? `Tool shell · ${toolShell.command.slice(0, 48)}${toolShell.command.length > 48 ? "…" : ""}`
                    : "Terminal"}
                </span>
                <button
                  type="button"
                  className="terminal-slot-collapse"
                  title="Collapse terminal"
                  onClick={() => {
                    setShowTerm(false);
                    setTermPeek(Boolean(toolShell));
                  }}
                >
                  Collapse
                </button>
              </div>
            )}
            <TerminalPane toolShell={toolShell} visible={showTerm} />
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
                  onClick={() => updateComposer(c.cmd + " ")}
                >
                  <strong>{c.cmd}</strong>
                  <span className="slash-desc">{c.desc}</span>
                </button>
              ))}
            </div>
          )}
          <div
            className={`composer-shell ${busy ? "is-busy" : ""} ${composerExpanded ? "is-expanded" : ""}`}
          >
            {activeTab && (
              <div className="composer-target" title="Composer sends to this session">
                <span className="composer-target-label">→</span>
                <span className={`kind-chip ${activeSummary?.kind ?? workspaceMode}`}>
                  {activeSummary?.kind ?? workspaceMode}
                </span>
                <span className="composer-target-title">{activeTab.title}</span>
                {docks.length > 1 && (
                  <span className="composer-target-zone">
                    zone{" "}
                    {Math.max(1, docks.indexOf(activeTab.id) + 1)}
                  </span>
                )}
              </div>
            )}
            <textarea
              className="composer-input"
              value={composer}
              rows={composerExpanded ? 10 : 2}
              placeholder={
                busy
                  ? "Turn running — Enter queues · Interject sends now"
                  : workspaceMode === "chat"
                    ? "Message Grok… (drafts keep per session · Shift+Enter newline)"
                    : "Message the coding agent… (drafts keep per session · Shift+Enter newline)"
              }
              onChange={(e) => updateComposer(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  // #147: Enter queues while busy (same as Send); Shift+Enter newline.
                  void sendPrompt();
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
                  <StyledSelect
                    aria-label="Model"
                    className="composer-select"
                    value={
                      models.some((m) => m.id === status?.model)
                        ? (status?.model ?? models[0]?.id ?? "grok-build")
                        : (models[0]?.id ?? status?.model ?? "grok-build")
                    }
                    options={models.map((m) => ({
                      value: m.id,
                      label: m.display_name,
                    }))}
                    onChange={(v) => {
                      void (async () => {
                        await api.setModel(v);
                        await refreshChrome();
                      })();
                    }}
                  />
                </label>
                <label
                  className="composer-pill"
                  title="Effort (default from Settings)"
                >
                  <span className="composer-pill-label">Effort</span>
                  <StyledSelect
                    aria-label="Effort"
                    className="composer-select"
                    value={String(status?.effort ?? "medium")}
                    options={[
                      "none",
                      "minimal",
                      "low",
                      "medium",
                      "high",
                      "xhigh",
                      "max",
                    ].map((e) => ({ value: e, label: e }))}
                    onChange={(v) => {
                      void (async () => {
                        await api.setEffort(v);
                        await refreshChrome();
                      })();
                    }}
                  />
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
                  title={
                    showTerm
                      ? "Collapse terminal"
                      : "Show terminal (collapsed by default during tool shells)"
                  }
                  onClick={() => {
                    setShowTerm((v) => {
                      const next = !v;
                      if (next) {
                        setTermEverOpened(true);
                        setTermPeek(false);
                      } else if (toolShell) {
                        setTermPeek(true);
                      }
                      return next;
                    });
                  }}
                >
                  Term
                </button>
                <button
                  type="button"
                  className={`composer-chip ${composerExpanded ? "on" : ""}`}
                  title={
                    composerExpanded
                      ? "Compact composer"
                      : "Expand composer for longer prompts"
                  }
                  onClick={() => setComposerExpanded((v) => !v)}
                >
                  {composerExpanded ? "Compact" : "Expand"}
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
                {busy && composer.trim() && (
                  <button
                    type="button"
                    className="composer-chip"
                    title="Cancel current turn and send this prompt now (#147)"
                    onClick={() => void sendPrompt(undefined, { interject: true })}
                  >
                    Interject
                  </button>
                )}
                {activeSessionId &&
                  (promptQueues[activeSessionId]?.length ?? 0) > 0 && (
                    <span
                      className="composer-hint"
                      title="Prompts waiting for the current turn to finish"
                    >
                      Queue {promptQueues[activeSessionId].length}
                    </span>
                  )}
                <button
                  type="button"
                  className="composer-send"
                  disabled={!composer.trim()}
                  title={
                    busy
                      ? "Queue prompt (turn running) · Interject to send now"
                      : "Send (Enter) · newline with Shift+Enter"
                  }
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

      <aside
        className={`rightbar ${rightbarCollapsed ? "is-collapsed" : ""}`}
        aria-expanded={!rightbarCollapsed}
      >
        <button
          type="button"
          className="rail-expand"
          title="Show tools panel (⌘⌥B)"
          aria-label="Show tools panel"
          onClick={() => setRightbarCollapsed(false)}
        >
          <span className="rail-expand-chevron" aria-hidden>
            ◂
          </span>
          <span className="rail-expand-label">Tools</span>
        </button>
        <div className="panel-body">
        <div className="panel-chrome">
          <button
            type="button"
            className="panel-collapse-btn"
            title="Hide tools panel (⌘⌥B)"
            aria-label="Hide tools panel"
            onClick={() => setRightbarCollapsed(true)}
          >
            ▸
          </button>
          <span className="panel-chrome-title">Tools</span>
        </div>
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
            <button
              type="button"
              onClick={async () => {
                const path = await api.lastEditedPath();
                if (path) {
                  setGitDiff(await api.agentEditDiffs());
                  setRightTab("git");
                  // Surface path in status for one-click last edit
                  setGitStatus((prev) => `Last edit: ${path}\n${prev || ""}`);
                } else {
                  setGitDiff("(no agent edits this process)");
                }
              }}
            >
              Open last edit
            </button>
            <button
              type="button"
              onClick={async () => {
                if (!activeSessionId) return;
                const text = await api.exportTranscript(activeSessionId);
                try {
                  await navigator.clipboard.writeText(text);
                  setGitDiff(`(transcript copied · ${text.length} chars)\n\n${text.slice(0, 4000)}`);
                } catch {
                  setGitDiff(text);
                }
              }}
            >
              Export transcript
            </button>
            <div className="panel-block">
              <strong>Worktrees</strong>
              <pre>{worktrees || "(none)"}</pre>
              <div className="worktree-create">
                <input
                  placeholder="Path (e.g. ../wt-feature)"
                  value={wtPath}
                  onChange={(e) => setWtPath(e.target.value)}
                  disabled={wtBusy}
                />
                <input
                  placeholder="New branch (optional)"
                  value={wtBranch}
                  onChange={(e) => setWtBranch(e.target.value)}
                  disabled={wtBusy}
                />
                <div className="worktree-create-actions">
                  <button
                    type="button"
                    disabled={wtBusy || !wtPath.trim()}
                    onClick={() => {
                      void (async () => {
                        setWtBusy(true);
                        try {
                          const msg = await api.createWorktree(
                            wtPath.trim(),
                            wtBranch.trim() || null,
                          );
                          setWorktrees(await api.listWorktrees());
                          setGitDiff(msg);
                        } catch (e) {
                          setGitDiff(String(e));
                        } finally {
                          setWtBusy(false);
                        }
                      })();
                    }}
                  >
                    Create worktree
                  </button>
                  <button
                    type="button"
                    disabled={wtBusy || !wtPath.trim()}
                    title="Switch project cwd to this worktree path"
                    onClick={() => {
                      void (async () => {
                        setWtBusy(true);
                        try {
                          const root = status?.project_cwd;
                          const abs = wtPath.trim().startsWith("/")
                            ? wtPath.trim()
                            : root
                              ? `${root.replace(/\/$/, "")}/${wtPath.trim()}`
                              : wtPath.trim();
                          await api.setProjectCwd(abs);
                          await refreshChrome();
                          setWorktrees(await api.listWorktrees());
                          setGitDiff(`Opened worktree as project: ${abs}`);
                        } catch (e) {
                          setGitDiff(String(e));
                        } finally {
                          setWtBusy(false);
                        }
                      })();
                    }}
                  >
                    Open as project
                  </button>
                  <button
                    type="button"
                    className="danger"
                    disabled={wtBusy || !wtPath.trim()}
                    onClick={() => {
                      void (async () => {
                        if (
                          !window.confirm(
                            `Remove worktree at ${wtPath.trim()}? Branch is kept.`,
                          )
                        ) {
                          return;
                        }
                        setWtBusy(true);
                        try {
                          const msg = await api.removeWorktree(wtPath.trim());
                          setWorktrees(await api.listWorktrees());
                          setGitDiff(msg || "Worktree removed");
                        } catch (e) {
                          setGitDiff(String(e));
                        } finally {
                          setWtBusy(false);
                        }
                      })();
                    }}
                  >
                    Remove
                  </button>
                </div>
              </div>
            </div>
            <button type="button" onClick={() => void api.gitStageAll()}>
              Stage all
            </button>
            <button
              type="button"
              onClick={() => {
                const msg = window.prompt("Commit message", "");
                if (msg == null || !msg.trim()) return;
                void api.gitCommit(msg.trim());
              }}
            >
              Commit…
            </button>
          </>
        )}

        {rightTab === "mcp" && (
          <>
            <div className="panel-block">
              <strong>Project MCP trust</strong>
              <p style={{ fontSize: 12, color: "var(--muted)", margin: "0.35rem 0" }}>
                Repo-local <code>.mcp.json</code> only runs after you trust this
                project. User-global servers under <code>~/.grokptah</code> are
                unaffected.
              </p>
              <button
                type="button"
                onClick={async () => {
                  const t = await api.mcpProjectTrust();
                  if (t.has_local_mcp && !t.trusted) {
                    await api.mcpSetProjectTrust(true);
                  } else if (t.trusted) {
                    await api.mcpSetProjectTrust(false);
                  } else {
                    setMcpTrustPrompt(t);
                  }
                  setMcpDoctor(await api.mcpDoctor());
                }}
              >
                Toggle project trust
              </button>
            </div>
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
            <div className="section-title">Multi-agent</div>
            <p style={{ fontSize: 11, color: "var(--muted)", margin: "0 0 0.5rem" }}>
              Parallel children (explore / general-purpose / plan) — not a flat
              status dump (#152).
            </p>
            {subagents.length === 0 && (
              <div className="panel-block" style={{ color: "var(--muted)" }}>
                (no subagents this session)
              </div>
            )}
            <div className="subagent-grid">
              {subagents
                .filter(
                  (a) =>
                    !activeSessionId ||
                    !a.session_id ||
                    a.session_id === activeSessionId,
                )
                .map((a) => (
                <div
                  key={a.id}
                  className={`subagent-card is-${String(a.status).toLowerCase().replace(/\s+/g, "-")}`}
                  data-testid="subagent-card"
                >
                  <div className="subagent-card-kind">{a.kind || "agent"}</div>
                  <div className="subagent-card-title">
                    {a.title || a.id.slice(0, 8)}
                  </div>
                  <div className="subagent-card-status">{a.status}</div>
                  {a.summary && (
                    <div className="subagent-card-summary" title={a.summary}>
                      {String(a.summary).slice(0, 160)}
                      {String(a.summary).length > 160 ? "…" : ""}
                    </div>
                  )}
                  {a.last_tool && (
                    <div className="subagent-card-tool">tool: {a.last_tool}</div>
                  )}
                  {String(a.status) === "running" && (
                    <button
                      type="button"
                      className="danger"
                      data-testid="subagent-cancel"
                      title="Cancel this child only"
                      onClick={() => {
                        void (async () => {
                          await api.cancelSubagent(a.id);
                          setSubagents(await api.subagentsList());
                        })();
                      }}
                    >
                      Cancel child
                    </button>
                  )}
                </div>
              ))}
            </div>
            <div className="section-title">Background / scheduled</div>
            <p style={{ fontSize: 11, color: "var(--muted)", margin: "0 0 0.5rem" }}>
              Long-running work outside the transcript (#52). Shell tool runs
              appear here; schedule a project scan or <code>!cmd</code> shell.
            </p>
            {bgTasks.length === 0 && (
              <div className="panel-block" style={{ color: "var(--muted)" }}>
                (no background tasks)
              </div>
            )}
            {bgTasks.map((t) => (
              <div key={t.id} className="panel-block bg-task-card">
                <div style={{ fontWeight: 600 }}>
                  {t.kind ? `[${t.kind}] ` : ""}
                  {t.title}
                </div>
                <div style={{ fontSize: 11, color: "var(--muted)" }}>
                  {t.status}
                  {t.detail && t.detail !== t.status ? ` · ${t.detail}` : ""}
                </div>
                <div className="worktree-create-actions" style={{ marginTop: 6 }}>
                  {String(t.status).startsWith("running") && (
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
                  {t.session_id && (
                    <button
                      type="button"
                      title="Focus owning session and show tool shell if live"
                      onClick={() => {
                        void (async () => {
                          try {
                            const s = await api.sessionLoad(String(t.session_id));
                            await openTab(s, true);
                            // Surface shell work outside the transcript.
                            setRightTab("tasks");
                            if (t.kind === "shell" || String(t.id).startsWith("shell-")) {
                              setShowTerm(true);
                              setTermEverOpened(true);
                              setTermPeek(false);
                              setToolShell({
                                callId: String(t.id).replace(/^shell-/, ""),
                                command: String(t.title),
                              });
                            }
                          } catch (e) {
                            console.warn(e);
                          }
                        })();
                      }}
                    >
                      Open session
                    </button>
                  )}
                </div>
              </div>
            ))}
            <button
              type="button"
              onClick={async () => {
                await api.scheduleBackgroundTask("Project file scan");
                setBgTasks(await api.backgroundTasks());
                setRightTab("tasks");
              }}
            >
              Schedule scan
            </button>
            <button
              type="button"
              onClick={async () => {
                const cmd = window.prompt(
                  "Shell command for background task (runs in project cwd)",
                  "sleep 2 && echo done",
                );
                if (!cmd?.trim()) return;
                await api.scheduleBackgroundTask(`!${cmd.trim()}`);
                setBgTasks(await api.backgroundTasks());
              }}
            >
              Schedule shell…
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
        </div>
      </aside>

      <footer className="status-bar">
        <span className={busy || anyBusy ? "status-live" : "status-idle"}>
          {busy
            ? `● Live · ${activity.label}${activity.detail ? ` — ${activity.detail}` : ""}`
            : anyBusy
              ? `○ Idle here · ${tabs.filter((t) => t.busy).length} other tab(s) active`
              : "○ Idle · ready"}
          {" · "}
          {status?.running ? "agent up" : "agent stopped"} · safety{" "}
          {status?.sandbox_profile ?? "—"}
        </span>
        <span>
          auto-update: {product.autoUpdateEnabled ? "on" : "off"} · bridge{" "}
          {product.bridgeVersion}
        </span>
      </footer>

      {mcpTrustPrompt && (
        <div className="modal-backdrop">
          <div className="modal permission-modal">
            <h3>Trust project MCP servers?</h3>
            <p>
              This project declares MCP servers in a local config (e.g.{" "}
              <code>.mcp.json</code>). Starting them runs commands from that
              repo — only trust projects you control.
            </p>
            {mcpTrustPrompt.project && (
              <p style={{ fontSize: 12, color: "var(--muted)", marginTop: 0 }}>
                Project: <code>{mcpTrustPrompt.project}</code>
              </p>
            )}
            <div className="modal-actions">
              <button
                type="button"
                onClick={() => setMcpTrustPrompt(null)}
              >
                Not now
              </button>
              <button
                type="button"
                className="danger"
                onClick={async () => {
                  await api.mcpSetProjectTrust(false);
                  setMcpTrustPrompt(null);
                }}
              >
                Never for this project
              </button>
              <button
                type="button"
                className="primary"
                onClick={async () => {
                  await api.mcpSetProjectTrust(true);
                  setMcpTrustPrompt(null);
                  if (rightTab === "mcp") {
                    setMcp(await api.mcpList());
                    setMcpDoctor(await api.mcpDoctor());
                  }
                }}
              >
                Trust & allow MCP
              </button>
            </div>
          </div>
        </div>
      )}

      {permission && (
        <PermissionModal
          request={permission}
          queuedBehind={Math.max(0, permissionQueue.length - 1)}
          fallbackSessionId={activeSessionId}
          onRespond={async (requestId, decision, sessionId) => {
            await api.permissionRespond(requestId, decision);
            setPermissionQueue((q) => dequeuePermission(q, requestId));
            // Patch the *owning* session, not whichever tab is focused (#141).
            if (sessionId) {
              patchTab(sessionId, (t) => ({
                ...t,
                needsPermission: false,
                activity: {
                  ...t.activity,
                  phase: "tool",
                  label: "Working",
                  detail:
                    decision === "deny" ? "Permission denied" : "Continuing…",
                  live: true,
                  lastEventAt: Date.now(),
                },
              }));
            }
            if (decision === "always_allow") {
              await refreshChrome();
            }
          }}
        />
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
  setPermissionQueue: React.Dispatch<
    React.SetStateAction<PermissionRequest[]>
  >,
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
                // Do not collapseRepeatedText here — it can drop legitimate
                // repeated code lines mid-stream. Multi-listener dups are
                // handled by mergeAssistantChunk / the session bus singleton.
                text: u.text,
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
      // Enqueue — concurrent requests must not overwrite the modal (#141).
      setPermissionQueue((q) => enqueuePermission(q, u.request));
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
    case "file_edit":
      withTab(sid!, (tab) => {
        const next = mapTranscript(
          tab,
          (t) => [
            ...t,
            {
              kind: "tool" as const,
              callId: `edit-${u.path}-${t.length}`,
              title: `edit ${u.path}`,
              status: "completed",
              output: u.summary,
            },
          ],
          { busy: true },
        );
        return withActivity(next, {
          phase: "tool",
          label: "Edit",
          detail: u.path,
          live: true,
        });
      });
      break;
    case "agent_progress":
      withTab(sid!, (tab) =>
        withActivity(
          {
            ...tab,
            busy: true,
            agentRound: u.round,
            lastTool: u.last_tool ?? tab.lastTool ?? null,
          },
          {
            phase: "tool",
            label: u.last_tool ? "Tool" : "Round",
            detail:
              u.detail ||
              (u.last_tool
                ? `${u.last_tool} · ${u.round}/${u.max_rounds}`
                : `round ${u.round}/${u.max_rounds}`),
            live: true,
          },
        ),
      );
      break;
    case "rate_limited":
      withTab(sid!, (tab) => {
        const next = mapTranscript(
          tab,
          (t) => [
            ...t,
            {
              kind: "error" as const,
              text: `Rate limited: ${u.message}${
                u.retry_after_ms
                  ? ` (retry ~${Math.ceil(u.retry_after_ms / 1000)}s)`
                  : ""
              }`,
            },
          ],
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
