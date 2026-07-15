import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { api } from "../lib/api";

type PtyOutput = { id: string; data: string };

export type ToolShellAttach = {
  callId: string;
  command: string;
};

/**
 * Multi-tab interactive PTY + tool-shell stream attach.
 *
 * Tool shells are attached by streaming output events from the *same*
 * process the agent spawned — never by re-executing the command.
 */
export function TerminalPane({
  toolShell,
}: {
  toolShell?: ToolShellAttach | null;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [tabs, setTabs] = useState<string[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [mode, setMode] = useState<"pty" | "tool">("pty");
  const activeIdRef = useRef<string | null>(null);
  const toolCallIdRef = useRef<string | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);

  useEffect(() => {
    activeIdRef.current = activeId;
  }, [activeId]);

  const refreshTabs = useCallback(async () => {
    setTabs(await api.ptyList());
  }, []);

  useEffect(() => {
    if (!hostRef.current) return;
    const term = new Terminal({
      theme: {
        background: "#050807",
        foreground: "#e7f2ec",
        cursor: "#2dd4a8",
      },
      fontFamily: "IBM Plex Mono, Menlo, monospace",
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
    let unlistenSession: (() => void) | undefined;

    (async () => {
      try {
        const id = await api.ptyCreate(term.cols, term.rows);
        if (disposed) {
          await api.ptyKill(id);
          return;
        }
        setActiveId(id);
        activeIdRef.current = id;
        setMode("pty");
        await refreshTabs();
        term.writeln(`\x1b[32mGrokPtah terminal\x1b[0m — interactive PTY ${id.slice(0, 8)}`);

        term.onData((data) => {
          // Only forward keystrokes to interactive PTY, not tool stream view
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

        unlistenPty = await listen<PtyOutput>("pty://output", (ev) => {
          if (modeRef.current !== "pty") return;
          const payload = ev.payload;
          if (payload.id === activeIdRef.current) {
            term.write(payload.data);
          }
        });

        // Session updates are also forwarded via parent; listen raw for shell stream
        unlistenSession = await listen("session://update", (ev) => {
          const raw = ev.payload as Record<string, unknown>;
          let u: Record<string, unknown> | null = null;
          if (typeof raw?.type === "string") {
            u = raw;
          } else {
            const k = Object.keys(raw || {})[0];
            if (k && raw[k] && typeof raw[k] === "object") {
              u = { type: k, ...(raw[k] as Record<string, unknown>) };
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
      unlistenSession?.();
      term.dispose();
      const cur = activeIdRef.current;
      if (cur) void api.ptyKill(cur);
    };
  }, [refreshTabs]);

  const modeRef = useRef<"pty" | "tool">("pty");
  useEffect(() => {
    modeRef.current = mode;
  }, [mode]);

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
    setActiveId(id);
    activeIdRef.current = id;
    if (clear) term.reset();
    try {
      const backlog = await api.ptyBacklog(id);
      if (backlog) term.write(backlog);
      else term.writeln(`\x1b[90m[tab ${id.slice(0, 8)}]\x1b[0m`);
      await api.ptyResize(id, term.cols, term.rows);
    } catch (e) {
      term.writeln(String(e));
    }
    await refreshTabs();
  }

  async function newTab() {
    const term = termRef.current;
    if (!term) return;
    const id = await api.ptyCreate(term.cols, term.rows);
    await switchTo(id, true);
  }

  async function closeActive() {
    const id = activeIdRef.current;
    if (!id) return;
    await api.ptyKill(id);
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
    <div>
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
          {mode === "tool" ? "Tool shell (live)" : "Terminal:"}
        </span>
        {tabs.map((t) => (
          <button
            key={t}
            type="button"
            className={t === activeId && mode === "pty" ? "warn-pill" : ""}
            onClick={() => void switchTo(t, true)}
          >
            {t.slice(0, 8)}
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
