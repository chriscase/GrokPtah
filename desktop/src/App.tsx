import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
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
import { StreamingText } from "./components/StreamingText";
import { TerminalPane, type ToolShellAttach } from "./components/TerminalPane";

type RightTab =
  | "files"
  | "git"
  | "mcp"
  | "plugins"
  | "skills"
  | "settings"
  | "tasks"
  | "rules";

function emptyTab(id: string, title = "New session"): SessionTab {
  return { id, title, transcript: [], busy: false, plan: null };
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
  const [settings, setSettings] = useState<Record<string, unknown>>({});
  const [rules, setRules] = useState<string[]>([]);
  const [product, setProduct] = useState({
    name: "GrokPtah",
    bridgeVersion: "?",
    autoUpdateEnabled: false,
  });
  const [showTerm, setShowTerm] = useState(false);
  const [toolShell, setToolShell] = useState<ToolShellAttach | null>(null);
  const [aboutOpen, setAboutOpen] = useState(false);
  const [apiKeyInput, setApiKeyInput] = useState("");
  const bottomRef = useRef<HTMLDivElement>(null);

  const activeTab = useMemo(
    () => tabs.find((t) => t.id === activeSessionId) ?? null,
    [tabs, activeSessionId],
  );
  const transcript = activeTab?.transcript ?? [];
  const busy = activeTab?.busy ?? false;
  const plan = activeTab?.plan ?? null;
  const anyBusy = tabs.some((t) => t.busy);

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

  const closeTab = useCallback(
    (id: string) => {
      setTabs((prev) => {
        const next = prev.filter((t) => t.id !== id);
        setActiveSessionId((cur) => {
          if (cur !== id) return cur;
          return next[next.length - 1]?.id ?? null;
        });
        return next;
      });
    },
    [],
  );

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

  useEffect(() => {
    void refreshChrome();
    let unlisten: (() => void) | undefined;
    listen("session://update", (event) => {
      const u = normalizeSessionUpdate(event.payload);
      if (!u) return;
      // Attach terminal to the *existing* tool shell stream — never re-exec.
      if (u.type === "shell_session_started") {
        setShowTerm(true);
        setToolShell({ callId: u.call_id, command: u.command });
      }
      applyUpdate(u, setTabs, setPermission);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, [refreshChrome]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [transcript, activeSessionId]);

  const slashOpen = composer.startsWith("/") && !composer.includes(" ");
  const slashHits = useMemo(
    () =>
      SLASH_COMMANDS.filter((c) =>
        c.cmd.startsWith(composer || "/"),
      ),
    [composer],
  );

  async function ensureSession(): Promise<string> {
    if (activeSessionId) return activeSessionId;
    const s = await api.sessionNew();
    await openTab(s, false);
    setSessions(await api.sessionList());
    return s.id;
  }

  async function openProject() {
    const path = await api.pickProjectFolder();
    if (path) {
      await refreshChrome();
      try {
        setFiles(await api.fileTree());
      } catch {
        /* empty */
      }
    }
  }

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
      title:
        t.title === "New session"
          ? prompt.slice(0, 48)
          : t.title,
      transcript: [...t.transcript, { kind: "user", text: prompt }],
    }));
    try {
      if (prompt === "/compact") {
        await api.sessionCompact(id);
        patchTab(id, (t) => ({
          ...t,
          busy: false,
          transcript: [
            ...t.transcript,
            { kind: "assistant", text: "Conversation compacted." },
          ],
        }));
        return;
      }
      const reply = await api.sessionPrompt(id, prompt);
      // Always surface the final reply (events may have streamed already).
      if (reply?.trim()) {
        patchTab(id, (t) => {
          const last = t.transcript[t.transcript.length - 1];
          if (
            last?.kind === "assistant" &&
            last.text.includes(reply.slice(0, 40))
          ) {
            return {
              ...t,
              busy: false,
              transcript: t.transcript.map((item, i) =>
                i === t.transcript.length - 1 && item.kind === "assistant"
                  ? { ...item, streaming: false }
                  : item,
              ),
            };
          }
          const already = t.transcript.some(
            (item) =>
              item.kind === "assistant" &&
              (item.text === reply ||
                reply.startsWith(item.text.slice(0, 80))),
          );
          if (already) {
            return {
              ...t,
              busy: false,
              transcript: t.transcript.map((item) =>
                item.kind === "assistant" || item.kind === "thought"
                  ? { ...item, streaming: false }
                  : item,
              ),
            };
          }
          return {
            ...t,
            busy: false,
            transcript: [
              ...t.transcript,
              { kind: "assistant", text: reply },
            ],
          };
        });
      } else {
        patchTab(id, (t) => ({
          ...t,
          busy: false,
          transcript: t.transcript.map((item) =>
            item.kind === "assistant" || item.kind === "thought"
              ? { ...item, streaming: false }
              : item,
          ),
        }));
      }
      setSessions(await api.sessionList());
      setSubagents(await api.subagentsList());
      setBgTasks(await api.backgroundTasks());
      await refreshChrome();
    } catch (e) {
      patchTab(id, (t) => ({
        ...t,
        busy: false,
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
      if (tab === "settings") setSettings(await api.settingsSnapshot());
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
            {status?.project_cwd ?? "no project open"}
          </span>
          <button type="button" onClick={() => void openProject()}>
            Open folder
          </button>
          <span
            className="path-chip"
            title={
              auth.signed_in
                ? `${auth.display_name} (${auth.method})`
                : "Not signed in — uses ~/.grok/auth.json from `grok login`, or API key"
            }
          >
            {auth.signed_in
              ? `auth: ${auth.method}`
              : "auth: none"}
          </span>
          {auth.signed_in ? (
            <button
              type="button"
              onClick={async () => setAuth(await api.signOut())}
            >
              Clear API key
            </button>
          ) : (
            <>
              <button
                type="button"
                onClick={async () => {
                  await api.authOpenLogin();
                }}
              >
                console.x.ai
              </button>
              <input
                type="password"
                placeholder="xAI API key (optional)"
                value={apiKeyInput}
                onChange={(e) => setApiKeyInput(e.target.value)}
                style={{
                  width: 150,
                  background: "var(--bg-input)",
                  border: "1px solid var(--border)",
                  borderRadius: 6,
                  padding: "0.25rem 0.4rem",
                  fontFamily: "var(--font)",
                  fontSize: 11,
                }}
              />
              <button
                type="button"
                className="primary"
                onClick={async () => {
                  if (!apiKeyInput.trim()) return;
                  setAuth(
                    await api.authSetApiKey(apiKeyInput.trim(), "API key"),
                  );
                  setApiKeyInput("");
                }}
              >
                Save key
              </button>
            </>
          )}
          <button type="button" onClick={() => setAboutOpen(true)}>
            About
          </button>
        </div>
      </header>

      <aside className="sidebar">
        <div className="section-title">Sessions</div>
        <button
          type="button"
          className="primary"
          style={{ width: "100%", marginBottom: 8 }}
          onClick={async () => {
            const s = await api.sessionNew();
            await openTab(s, false);
            setSessions(await api.sessionList());
          }}
        >
          New session
        </button>
        <p className="session-hint">
          Open several tabs and run builds in parallel — switch anytime.
        </p>
        {sessions.map((s) => {
          const open = tabs.some((t) => t.id === s.id);
          const running = tabs.find((t) => t.id === s.id)?.busy;
          return (
            <button
              key={s.id}
              type="button"
              className={`session-item ${s.id === activeSessionId ? "active" : ""} ${running ? "busy" : ""}`}
              onClick={async () => {
                await api.sessionLoad(s.id);
                await openTab(s, !open);
              }}
            >
              <div className="session-item-title">
                {running && <span className="busy-dot" title="Running" />}
                {s.title}
              </div>
              <div style={{ color: "var(--muted)", fontSize: 11 }}>
                {s.message_count} msgs{open ? " · open" : ""}
              </div>
            </button>
          );
        })}
        <div className="section-title">Session actions</div>
        <button
          type="button"
          disabled={!activeSessionId}
          onClick={async () => {
            if (!activeSessionId) return;
            const f = await api.sessionFork(activeSessionId);
            await openTab(f, false);
            // Copy transcript into forked tab from source if still open
            const src = tabs.find((t) => t.id === activeSessionId);
            if (src) {
              patchTab(f.id, (t) => ({
                ...t,
                transcript: [...src.transcript],
                title: f.title,
              }));
            }
            setSessions(await api.sessionList());
          }}
        >
          Fork
        </button>
        <button
          type="button"
          disabled={!activeSessionId}
          onClick={async () => {
            if (!activeSessionId) return;
            await api.sessionRewind(activeSessionId, 1);
            setSessions(await api.sessionList());
            await openTab(
              sessions.find((s) => s.id === activeSessionId) ?? {
                id: activeSessionId,
                title: activeTab?.title ?? "Session",
                cwd: "",
                created_at: "",
                updated_at: "",
                message_count: 0,
              },
              true,
            );
          }}
        >
          Rewind
        </button>
        <button
          type="button"
          disabled={!activeSessionId}
          onClick={async () => {
            if (!activeSessionId) return;
            await api.sessionCompact(activeSessionId);
          }}
        >
          Compact
        </button>
        <button type="button" onClick={() => setShowTerm((v) => !v)}>
          {showTerm ? "Hide terminal" : "Show terminal"}
        </button>
      </aside>

      <main className="main">
        {tabs.length > 0 && (
          <div className="session-tabs" role="tablist" aria-label="Open sessions">
            {tabs.map((t) => (
              <div
                key={t.id}
                className={`session-tab ${t.id === activeSessionId ? "active" : ""} ${t.busy ? "busy" : ""}`}
                role="tab"
                aria-selected={t.id === activeSessionId}
              >
                <button
                  type="button"
                  className="session-tab-label"
                  onClick={() => setActiveSessionId(t.id)}
                  title={t.title}
                >
                  {t.busy && <span className="busy-dot" />}
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
              title="New session tab"
              onClick={async () => {
                const s = await api.sessionNew();
                await openTab(s, false);
                setSessions(await api.sessionList());
              }}
            >
              +
            </button>
          </div>
        )}
        <div className="transcript">
          {transcript.length === 0 && (
            <div className="empty-agent">
              <h1>GrokPtah</h1>
              <div className="version-line">
                Grok Build as a desktop agent · bridge{" "}
                {product.bridgeVersion}
              </div>
              <ul>
                <li>
                  Auth: reuses <code>~/.grok/auth.json</code> from{" "}
                  <code>grok login</code> (or paste an API key)
                </li>
                <li>
                  Open a project folder, then type a prompt below
                </li>
                <li>
                  Multi-session: open several tabs and run builds in
                  parallel (like Claude Code)
                </li>
                <li>
                  Slash: <code>/help</code> <code>/plan</code>{" "}
                  <code>/yolo</code> · tools: <code>list files</code>,{" "}
                  <code>run …</code>
                </li>
              </ul>
            </div>
          )}
          {transcript.map((item, i) => (
            <div key={i} className={`bubble ${item.kind}`}>
              {item.kind === "tool" && (
                <>
                  <strong>
                    {item.title} · {item.status}
                  </strong>
                  {item.output && <pre>{item.output}</pre>}
                </>
              )}
              {item.kind === "plan" && (
                <>
                  <strong>Plan ({item.status})</strong>
                  <ol>
                    {item.steps.map((s, j) => (
                      <li key={j}>{s}</li>
                    ))}
                  </ol>
                  {activeSessionId && item.status === "proposed" && (
                    <div className="modal-actions">
                      <button
                        type="button"
                        className="primary"
                        onClick={() => void api.acceptPlan(activeSessionId)}
                      >
                        Accept
                      </button>
                      <button
                        type="button"
                        onClick={() => void api.rejectPlan(activeSessionId)}
                      >
                        Reject
                      </button>
                    </div>
                  )}
                </>
              )}
              {(item.kind === "assistant" || item.kind === "thought") && (
                <StreamingText text={item.text} streaming={item.streaming} />
              )}
              {(item.kind === "user" || item.kind === "error") && item.text}
            </div>
          ))}
          <div ref={bottomRef} />
        </div>

        {showTerm && <TerminalPane toolShell={toolShell} />}

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
                  <strong>{c.cmd}</strong> — {c.desc}
                </button>
              ))}
            </div>
          )}
          <div className="composer-meta">
            <select
              value={status?.model ?? "grok-build"}
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
            <select
              value={String(status?.effort ?? "medium")}
              onChange={async (e) => {
                await api.setEffort(e.target.value);
                await refreshChrome();
              }}
            >
              {["none", "minimal", "low", "medium", "high", "xhigh", "max"].map(
                (e) => (
                  <option key={e} value={e}>
                    effort: {e}
                  </option>
                ),
              )}
            </select>
            <label>
              <input
                type="checkbox"
                checked={!!status?.always_approve}
                onChange={async (e) => {
                  await api.setAlwaysApprove(e.target.checked);
                  await refreshChrome();
                }}
              />{" "}
              <span className={status?.always_approve ? "yolo-on" : ""}>
                Always approve
              </span>
            </label>
            {busy && (
              <button
                type="button"
                className="danger"
                onClick={() =>
                  void api.sessionCancel(activeSessionId)
                }
              >
                Stop
              </button>
            )}
            {anyBusy && !busy && (
              <span className="muted-chip" title="Other session tabs are still running">
                {tabs.filter((t) => t.busy).length} other tab
                {tabs.filter((t) => t.busy).length === 1 ? "" : "s"} running
              </span>
            )}
          </div>
          <div className="composer-row">
            <textarea
              value={composer}
              placeholder={
                busy
                  ? "This session is running… switch tabs to start another"
                  : "Message GrokPtah… (Enter send, Shift+Enter newline)"
              }
              onChange={(e) => setComposer(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  if (!busy) void sendPrompt();
                }
              }}
            />
            <button
              type="button"
              className="primary"
              disabled={busy}
              onClick={() => void sendPrompt()}
            >
              Send
            </button>
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
              "settings",
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
              <pre>
                {/* loaded on demand */}
                {String((settings as any)._hooks || "Open settings refresh or click Load hooks")}
              </pre>
              <button
                type="button"
                onClick={async () => {
                  const h = await api.hooksConfig();
                  setSettings((s) => ({ ...s, _hooks: h }));
                }}
              >
                Load hooks
              </button>
            </div>
          </>
        )}

        {rightTab === "settings" && (
          <>
            <div className="panel-block">
              <pre>{JSON.stringify(settings, null, 2)}</pre>
            </div>
            <label>
              Sandbox{" "}
              <select
                onChange={async (e) => {
                  await api.setSandbox(e.target.value);
                  setSettings(await api.settingsSnapshot());
                }}
              >
                <option value="workspace-write">workspace-write</option>
                <option value="read-only">read-only</option>
                <option value="danger-full-access">danger-full-access</option>
              </select>
            </label>
            <label>
              Appearance{" "}
              <select
                onChange={async (e) => {
                  await api.setAppearance(e.target.value);
                  setSettings(await api.settingsSnapshot());
                }}
              >
                <option value="dark">dark</option>
                <option value="light">light</option>
              </select>
            </label>
            <label>
              Permission mode{" "}
              <select
                onChange={async (e) => {
                  await api.setPermissionMode(e.target.value);
                  setSettings(await api.settingsSnapshot());
                }}
              >
                <option value="default">default</option>
                <option value="bypassPermissions">bypassPermissions</option>
              </select>
            </label>
            <button
              type="button"
              onClick={async () => {
                await api.setAllowDenyRules(["Shell(*)"], ["WebFetch(*)"]);
                setSettings(await api.settingsSnapshot());
              }}
            >
              Apply sample allow/deny rules
            </button>
            {settings.autoUpdateEnabled === false && (
              <p className="warn-pill">Upstream CLI auto-update disabled</p>
            )}
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
        <span>
          {status?.running ? "Agent running (in-process)" : "Agent stopped"} ·{" "}
          {status?.sandbox_profile}
        </span>
        <span>
          auto-update: {product.autoUpdateEnabled ? "on" : "off"} · bridge{" "}
          {product.bridgeVersion}
        </span>
      </footer>

      {permission && (
        <div className="modal-backdrop">
          <div className="modal">
            <h3>Permission required</h3>
            <p>{permission.summary}</p>
            <pre style={{ fontSize: 12, color: "var(--muted)" }}>
              {JSON.stringify(permission.detail, null, 2)}
            </pre>
            <div className="modal-actions">
              <button
                type="button"
                className="danger"
                onClick={async () => {
                  await api.permissionRespond(permission.id, "deny");
                  setPermission(null);
                }}
              >
                Deny
              </button>
              <button
                type="button"
                onClick={async () => {
                  await api.permissionRespond(permission.id, "always_allow");
                  setPermission(null);
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
                }}
              >
                Allow
              </button>
            </div>
          </div>
        </div>
      )}

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
      withTab(sid!, (tab) =>
        mapTranscript(
          tab,
          (t) => {
            const last = t[t.length - 1];
            if (last?.kind === "assistant") {
              const copy = t.slice(0, -1);
              copy.push({
                kind: "assistant",
                text: last.text + u.text,
                streaming: true,
              });
              return copy;
            }
            return [
              ...t,
              { kind: "assistant", text: u.text, streaming: true },
            ];
          },
          { busy: true },
        ),
      );
      break;
    case "agent_thought_chunk":
      withTab(sid!, (tab) =>
        mapTranscript(
          tab,
          (t) => {
            const last = t[t.length - 1];
            if (last?.kind === "thought") {
              const copy = t.slice(0, -1);
              copy.push({
                kind: "thought",
                text: last.text + u.text,
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
        ),
      );
      break;
    case "tool_call":
      withTab(sid!, (tab) =>
        mapTranscript(
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
        ),
      );
      break;
    case "tool_call_update":
      withTab(sid!, (tab) =>
        mapTranscript(tab, (t) =>
          t.map((item) =>
            item.kind === "tool" && item.callId === u.call_id
              ? {
                  ...item,
                  status: u.status,
                  output: u.output ?? item.output,
                }
              : item,
          ),
        ),
      );
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
      break;
    case "turn_complete":
      withTab(sid!, (tab) => ({
        ...tab,
        busy: false,
        transcript: tab.transcript.map((item) =>
          item.kind === "assistant" || item.kind === "thought"
            ? { ...item, streaming: false }
            : item,
        ),
      }));
      break;
    case "error":
      withTab(sid!, (tab) =>
        mapTranscript(
          tab,
          (t) => [...t, { kind: "error", text: u.message }],
          { busy: false },
        ),
      );
      break;
    default:
      break;
  }
}
