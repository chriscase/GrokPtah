import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { api, type PtyBacklog } from "../lib/api";

type PtyOutput = { id: string; data: string; seq: number };
type PtyExit = { id: string };

export type ToolShellAttach = {
  callId: string;
  command: string;
};

/**
 * Multi-tab interactive PTY + tool-shell stream attach.
 *
 * #136: Parent should hide this pane with CSS rather than unmounting when
 * collapsing — we still avoid kill-on-unmount so remount can reattach.
 * Explicit "Close tab" is the only kill path for interactive shells.
 *
 * #138: seq watermarks prevent backlog+live double-render; exit events drop dead tabs.
 */
export function TerminalPane({
  toolShell,
  visible = true,
}: {
  toolShell?: ToolShellAttach | null;
  /** When false the pane is CSS-hidden; we refit on re-show without killing the shell. */
  visible?: boolean;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [tabs, setTabs] = useState<string[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [mode, setMode] = useState<"pty" | "tool">("pty");
  const activeIdRef = useRef<string | null>(null);
  const toolCallIdRef = useRef<string | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const modeRef = useRef<"pty" | "tool">("pty");
  /** Highest seq already rendered per PTY id (from backlog or live). */
  const seqWatermarkRef = useRef<Map<string, number>>(new Map());
  /**
   * While applying backlog for an id, buffer live events instead of painting
   * them (avoids live-then-full-backlog duplicates). After paint, flush any
   * queued events with seq > watermark so nothing is lost (#138).
   */
  const applyingBacklogRef = useRef<Set<string>>(new Set());
  const pendingLiveRef = useRef<Map<string, PtyOutput[]>>(new Map());

  useEffect(() => {
    activeIdRef.current = activeId;
  }, [activeId]);

  useEffect(() => {
    modeRef.current = mode;
  }, [mode]);

  const refreshTabs = useCallback(async () => {
    setTabs(await api.ptyList());
  }, []);

  useEffect(() => {
    if (!visible) return;
    const fit = fitRef.current;
    const term = termRef.current;
    if (!fit || !term) return;
    // Collapse/expand via CSS: refit so the PTY matches the restored host size.
    requestAnimationFrame(() => {
      fit.fit();
      const cur = activeIdRef.current;
      if (cur && modeRef.current === "pty") {
        void api.ptyResize(cur, term.cols, term.rows);
      }
    });
  }, [visible]);

  useEffect(() => {
    if (!hostRef.current) return;
    // #129: pull xterm colors from design tokens (amber accent, shared surfaces).
    const css = getComputedStyle(document.documentElement);
    const tok = (name: string, fb: string) =>
      (css.getPropertyValue(name).trim() || fb);
    const term = new Terminal({
      theme: {
        background: tok("--surface-deep", "#050608"),
        foreground: tok("--text", "#e8eaed"),
        cursor: tok("--accent", "#f0b429"),
        selectionBackground: tok("--accent-bg", "rgba(240,180,41,0.25)"),
      },
      fontFamily: tok(
        "--font-mono",
        "SF Mono, JetBrains Mono, IBM Plex Mono, ui-monospace, Menlo, monospace",
      ),
      fontSize: 12,
      convertEol: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(hostRef.current);
    fit.fit();
    termRef.current = term;
    fitRef.current = fit;

    let disposed = false;
    let unlistenPty: (() => void) | undefined;
    let unlistenExit: (() => void) | undefined;
    let unlistenSession: (() => void) | undefined;

    function paintLive(payload: PtyOutput, t: Terminal) {
      const mark = seqWatermarkRef.current.get(payload.id) ?? 0;
      if (payload.seq <= mark) return;
      seqWatermarkRef.current.set(payload.id, payload.seq);
      if (payload.id === activeIdRef.current && modeRef.current === "pty") {
        t.write(payload.data);
      }
    }

    function flushPending(id: string, t: Terminal) {
      const q = pendingLiveRef.current.get(id);
      pendingLiveRef.current.delete(id);
      if (!q) return;
      for (const p of q) paintLive(p, t);
    }

    /**
     * Fetch backlog, set seq watermark *before* writing, then paint once.
     * Concurrent live events are queued and flushed after (seq-filtered).
     */
    async function applyBacklog(id: string, t: Terminal, opts?: { banner?: string }) {
      applyingBacklogRef.current.add(id);
      pendingLiveRef.current.set(id, []);
      try {
        const bl: PtyBacklog = await api.ptyBacklog(id);
        // Watermark first so flushed live events drop dups already in backlog.
        seqWatermarkRef.current.set(id, bl.upToSeq);
        if (opts?.banner) t.writeln(opts.banner);
        if (bl.data) t.write(bl.data);
      } catch {
        /* empty / gone ok */
      } finally {
        applyingBacklogRef.current.delete(id);
        flushPending(id, t);
      }
    }

    (async () => {
      try {
        // Register listeners *before* backlog apply so chunks during attach
        // are queued rather than lost (#138).
        unlistenPty = await listen<PtyOutput>("pty://output", (ev) => {
          if (modeRef.current !== "pty") return;
          const payload = ev.payload;
          if (applyingBacklogRef.current.has(payload.id)) {
            const q = pendingLiveRef.current.get(payload.id) ?? [];
            q.push(payload);
            pendingLiveRef.current.set(payload.id, q);
            return;
          }
          paintLive(payload, term);
        });

        unlistenExit = await listen<PtyExit>("pty://exit", (ev) => {
          const dead = ev.payload.id;
          seqWatermarkRef.current.delete(dead);
          applyingBacklogRef.current.delete(dead);
          pendingLiveRef.current.delete(dead);
          void (async () => {
            const list = await api.ptyList();
            setTabs(list);
            if (activeIdRef.current === dead) {
              if (list.length) {
                const next = list[0];
                setActiveId(next);
                activeIdRef.current = next;
                term.reset();
                await applyBacklog(next, term, {
                  banner: `\x1b[90m[shell exited — switched tab]\x1b[0m`,
                });
              } else {
                setActiveId(null);
                activeIdRef.current = null;
                term.writeln("\r\n\x1b[90m[shell exited]\x1b[0m");
              }
            }
          })();
        });

        unlistenSession = await listen("session://update", (ev) => {
          const raw = ev.payload as Record<string, unknown>;
          let u: Record<string, unknown> | null = null;
          if (typeof raw?.type === "string") {
            u = raw;
          } else {
            const k = Object.keys(raw || {})[0];
            if (k && raw[k] && typeof raw[k] === "object") {
              u = { type: k, ...(raw[k] as object as Record<string, unknown>) };
            }
          }
          if (!u) return;
          if (u.type === "shell_output") {
            if (String(u.call_id) === toolCallIdRef.current) {
              term.write(String(u.data ?? ""));
            }
          }
          if (u.type === "shell_session_ended") {
            if (String(u.call_id) === toolCallIdRef.current) {
              const cancelled = Boolean(u.cancelled);
              term.writeln(
                cancelled
                  ? "\r\n\x1b[31m[tool shell cancelled]\x1b[0m"
                  : "\r\n\x1b[90m[tool shell ended]\x1b[0m",
              );
            }
          }
        });

        term.onData((data) => {
          if (modeRef.current === "pty") {
            const cur = activeIdRef.current;
            if (cur) void api.ptyWrite(cur, data);
          }
        });
        term.onResize(({ cols, rows }) => {
          const cur = activeIdRef.current;
          if (cur && modeRef.current === "pty") {
            void api.ptyResize(cur, cols, rows);
          }
        });

        // Prefer reattaching an existing PTY (collapse/expand) over creating a new one (#136).
        let id: string;
        const existing = await api.ptyList();
        if (existing.length > 0) {
          id = existing[0];
          setActiveId(id);
          activeIdRef.current = id;
          setMode("pty");
          await applyBacklog(id, term, {
            // Amber accent (not green third-family) — raw UUID not primary (#129).
            banner: `\x1b[33mTerminal\x1b[0m · reattached`,
          });
          await api.ptyResize(id, term.cols, term.rows);
        } else {
          id = await api.ptyCreate(term.cols, term.rows);
          if (disposed) {
            // Only kill if we created and then immediately unmounted.
            await api.ptyKill(id);
            return;
          }
          seqWatermarkRef.current.set(id, 0);
          setActiveId(id);
          activeIdRef.current = id;
          setMode("pty");
          term.writeln(`\x1b[33mTerminal\x1b[0m · ready`);
        }
        if (disposed) return;
        await refreshTabs();
      } catch (e) {
        term.writeln("PTY unavailable: " + String(e));
      }
    })();

    const onResize = () => fit.fit();
    window.addEventListener("resize", onResize);

    return () => {
      disposed = true;
      window.removeEventListener("resize", onResize);
      unlistenPty?.();
      unlistenExit?.();
      unlistenSession?.();
      term.dispose();
      // #136: do NOT kill PTY on unmount — collapse/hide must preserve shell.
      // Explicit Close tab still calls ptyKill.
      termRef.current = null;
      fitRef.current = null;
    };
  }, [refreshTabs]);

  // Attach to existing tool shell stream (display only — never re-exec).
  useEffect(() => {
    if (!toolShell || !termRef.current) return;
    const term = termRef.current;
    toolCallIdRef.current = toolShell.callId;
    setMode("tool");
    term.reset();
    term.writeln(
      `\x1b[36m[attached tool shell]\x1b[0m $ ${toolShell.command}\r\n\x1b[90m(streaming live process — not re-executed)\x1b[0m\r\n`,
    );
  }, [toolShell]);

  async function switchTo(id: string, clear = true) {
    const term = termRef.current;
    if (!term) return;
    setMode("pty");
    toolCallIdRef.current = null;
    // Queue live paint for this id while we clear/write backlog.
    applyingBacklogRef.current.add(id);
    pendingLiveRef.current.set(id, []);
    setActiveId(id);
    activeIdRef.current = id;
    if (clear) term.reset();
    try {
      const bl = await api.ptyBacklog(id);
      // Watermark *before* write so flushed live chunks skip dups.
      seqWatermarkRef.current.set(id, bl.upToSeq);
      if (bl.data) term.write(bl.data);
      else term.writeln(`\x1b[90m[tab]\x1b[0m`);
      await api.ptyResize(id, term.cols, term.rows);
    } catch (e) {
      term.writeln(String(e));
    } finally {
      applyingBacklogRef.current.delete(id);
      const q = pendingLiveRef.current.get(id);
      pendingLiveRef.current.delete(id);
      if (q) {
        for (const p of q) {
          const mark = seqWatermarkRef.current.get(p.id) ?? 0;
          if (p.seq <= mark) continue;
          seqWatermarkRef.current.set(p.id, p.seq);
          if (p.id === activeIdRef.current) term.write(p.data);
        }
      }
    }
    await refreshTabs();
  }

  async function newTab() {
    const term = termRef.current;
    if (!term) return;
    const id = await api.ptyCreate(term.cols, term.rows);
    seqWatermarkRef.current.set(id, 0);
    await switchTo(id, true);
  }

  async function closeActive() {
    const id = activeIdRef.current;
    if (!id) return;
    await api.ptyKill(id);
    seqWatermarkRef.current.delete(id);
    const list = await api.ptyList();
    setTabs(list);
    if (list.length) {
      await switchTo(list[0], true);
    } else {
      setActiveId(null);
      activeIdRef.current = null;
      termRef.current?.reset();
      await newTab();
    }
  }

  return (
    <div className="terminal-pane-root">
      <div
        style={{
          display: "flex",
          gap: 6,
          padding: "4px 8px",
          borderTop: "1px solid var(--border)",
          background: "var(--bg-elevated)",
          fontSize: 12,
          alignItems: "center",
          flexWrap: "wrap",
        }}
      >
        <span style={{ color: "var(--muted)" }}>
          {mode === "tool" ? "Tool shell (live)" : "Terminal"}
        </span>
        {tabs.map((t, i) => (
          <button
            key={t}
            type="button"
            className={t === activeId && mode === "pty" ? "warn-pill" : ""}
            title={`Session ${t.slice(0, 8)}…`}
            onClick={() => void switchTo(t, true)}
          >
            {/* #129: short labels — raw PTY UUIDs only in title tooltip */}
            Tab {i + 1}
          </button>
        ))}
        <button type="button" onClick={() => void newTab()}>
          New tab
        </button>
        <button type="button" onClick={() => void closeActive()}>
          Close tab
        </button>
      </div>
      <div className="term-host" ref={hostRef} />
    </div>
  );
}
