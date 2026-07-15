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
  type SessionUpdate,
} from "./lib/protocol";
import { TerminalPane } from "./components/TerminalPane";

type TranscriptItem =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "thought"; text: string }
  | {
      kind: "tool";
      callId: string;
      title: string;
      status: string;
      output?: string;
    }
  | { kind: "plan"; steps: string[]; status: string }
  | { kind: "error"; text: string };

type RightTab =
  | "files"
  | "git"
  | "mcp"
  | "plugins"
  | "skills"
  | "settings"
  | "tasks"
  | "rules";

export default function App() {
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [auth, setAuth] = useState<AuthState>({ signed_in: false });
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [transcript, setTranscript] = useState<TranscriptItem[]>([]);
  const [composer, setComposer] = useState("");
  const [busy, setBusy] = useState(false);
  const [permission, setPermission] = useState<PermissionRequest | null>(null);
  const [plan, setPlan] = useState<{ steps: string[]; status: string } | null>(
    null,
  );
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
  const [aboutOpen, setAboutOpen] = useState(false);
  const bottomRef = useRef<HTMLDivElement>(null);

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
      applyUpdate(u, setTranscript, setPermission, setPlan, setBusy);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, [refreshChrome]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [transcript]);

  const slashOpen = composer.startsWith("/") && !composer.includes(" ");
  const slashHits = useMemo(
    () =>
      SLASH_COMMANDS.filter((c) =>
        c.cmd.startsWith(composer || "/"),
      ),
    [composer],
  );

  async function ensureSession(): Promise<string> {
    if (sessionId) return sessionId;
    const s = await api.sessionNew();
    setSessionId(s.id);
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
    setTranscript((t) => [...t, { kind: "user", text: prompt }]);
    setBusy(true);
    try {
      const id = await ensureSession();
      if (prompt === "/compact") {
        await api.sessionCompact(id);
        setTranscript((t) => [
          ...t,
          { kind: "assistant", text: "Conversation compacted." },
        ]);
        setBusy(false);
        return;
      }
      await api.sessionPrompt(id, prompt);
      setSessions(await api.sessionList());
      setSubagents(await api.subagentsList());
      setBgTasks(await api.backgroundTasks());
    } catch (e) {
      setTranscript((t) => [
        ...t,
        { kind: "error", text: String(e) },
      ]);
      setBusy(false);
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
          <div className="brand-mark" />
          <span>{product.name}</span>
        </div>
        <div className="title-actions">
          <span>{status?.project_cwd ?? "No project"}</span>
          <button type="button" onClick={() => void openProject()}>
            Open folder
          </button>
          {auth.signed_in ? (
            <button
              type="button"
              onClick={async () => setAuth(await api.signOut())}
            >
              Sign out ({auth.display_name})
            </button>
          ) : (
            <button
              type="button"
              className="primary"
              onClick={async () =>
                setAuth(await api.signInLocal("GrokPtah User"))
              }
            >
              Sign in
            </button>
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
            setSessionId(s.id);
            setTranscript([]);
            setPlan(null);
            setSessions(await api.sessionList());
          }}
        >
          New session
        </button>
        {sessions.map((s) => (
          <button
            key={s.id}
            type="button"
            className={`session-item ${s.id === sessionId ? "active" : ""}`}
            onClick={async () => {
              await api.sessionLoad(s.id);
              setSessionId(s.id);
            }}
          >
            <div>{s.title}</div>
            <div style={{ color: "var(--muted)", fontSize: 11 }}>
              {s.message_count} msgs
            </div>
          </button>
        ))}
        <div className="section-title">Session actions</div>
        <button
          type="button"
          disabled={!sessionId}
          onClick={async () => {
            if (!sessionId) return;
            const f = await api.sessionFork(sessionId);
            setSessionId(f.id);
            setSessions(await api.sessionList());
          }}
        >
          Fork
        </button>
        <button
          type="button"
          disabled={!sessionId}
          onClick={async () => {
            if (!sessionId) return;
            await api.sessionRewind(sessionId, 1);
            setSessions(await api.sessionList());
          }}
        >
          Rewind
        </button>
        <button
          type="button"
          disabled={!sessionId}
          onClick={async () => {
            if (!sessionId) return;
            await api.sessionCompact(sessionId);
          }}
        >
          Compact
        </button>
        <button type="button" onClick={() => setShowTerm((v) => !v)}>
          {showTerm ? "Hide terminal" : "Show terminal"}
        </button>
      </aside>

      <main className="main">
        <div className="transcript">
          {transcript.length === 0 && (
            <div className="bubble thought">
              Welcome to GrokPtah. Open a project folder, then chat. Try{" "}
              <code>/help</code>, <code>list files</code>, or{" "}
              <code>make a plan</code>. Agent runs in-process (no stdio child).
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
                  {sessionId && item.status === "proposed" && (
                    <div className="modal-actions">
                      <button
                        type="button"
                        className="primary"
                        onClick={() => void api.acceptPlan(sessionId)}
                      >
                        Accept
                      </button>
                      <button
                        type="button"
                        onClick={() => void api.rejectPlan(sessionId)}
                      >
                        Reject
                      </button>
                    </div>
                  )}
                </>
              )}
              {item.kind !== "tool" && item.kind !== "plan" && item.text}
            </div>
          ))}
          <div ref={bottomRef} />
        </div>

        {showTerm && <TerminalPane />}

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
                onClick={() => void api.sessionCancel()}
              >
                Stop
              </button>
            )}
          </div>
          <div className="composer-row">
            <textarea
              value={composer}
              placeholder="Message GrokPtah… (Enter send, Shift+Enter newline)"
              onChange={(e) => setComposer(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  void sendPrompt();
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

        {rightTab === "skills" &&
          skills.map((s) => (
            <div key={s.id} className="panel-block">
              <strong>{s.name}</strong>
              <div style={{ color: "var(--muted)" }}>{s.description}</div>
            </div>
          ))}

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
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>{product.name}</h3>
            <p>
              Desktop coding agent · bridge {product.bridgeVersion}
              <br />
              Apache-2.0 · fork of xai-org/grok-build
              <br />
              Upstream CLI auto-update:{" "}
              {product.autoUpdateEnabled ? "enabled" : "disabled"}
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

function applyUpdate(
  u: SessionUpdate,
  setTranscript: React.Dispatch<React.SetStateAction<TranscriptItem[]>>,
  setPermission: React.Dispatch<React.SetStateAction<PermissionRequest | null>>,
  setPlan: React.Dispatch<
    React.SetStateAction<{ steps: string[]; status: string } | null>
  >,
  setBusy: React.Dispatch<React.SetStateAction<boolean>>,
) {
  switch (u.type) {
    case "agent_message_chunk":
      setTranscript((t) => {
        const last = t[t.length - 1];
        if (last?.kind === "assistant") {
          const copy = t.slice(0, -1);
          copy.push({ kind: "assistant", text: last.text + u.text });
          return copy;
        }
        return [...t, { kind: "assistant", text: u.text }];
      });
      break;
    case "agent_thought_chunk":
      setTranscript((t) => {
        const last = t[t.length - 1];
        if (last?.kind === "thought") {
          const copy = t.slice(0, -1);
          copy.push({ kind: "thought", text: last.text + u.text });
          return copy;
        }
        return [...t, { kind: "thought", text: u.text }];
      });
      break;
    case "tool_call":
      setTranscript((t) => [
        ...t,
        {
          kind: "tool",
          callId: u.call_id,
          title: u.title,
          status: u.status,
        },
      ]);
      break;
    case "tool_call_update":
      setTranscript((t) =>
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
      break;
    case "plan":
      setPlan({ steps: u.steps, status: u.status });
      setTranscript((t) => [
        ...t,
        { kind: "plan", steps: u.steps, status: u.status },
      ]);
      break;
    case "permission_required":
      setPermission(u.request);
      break;
    case "turn_complete":
      setBusy(false);
      break;
    case "error":
      setTranscript((t) => [...t, { kind: "error", text: u.message }]);
      break;
    default:
      break;
  }
}
