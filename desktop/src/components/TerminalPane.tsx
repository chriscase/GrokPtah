import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { api } from "../lib/api";

type PtyOutput = { id: string; data: string };

/**
 * Multi-tab terminal: each tab owns a PTY id; active tab receives output events
 * and keyboard input. Backlog is replayed on switch.
 */
export function TerminalPane({ attachCommand }: { attachCommand?: string | null }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [tabs, setTabs] = useState<string[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const activeIdRef = useRef<string | null>(null);
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
    let unlisten: (() => void) | undefined;

    (async () => {
      try {
        const id = await api.ptyCreate(term.cols, term.rows);
        if (disposed) {
          await api.ptyKill(id);
          return;
        }
        setActiveId(id);
        activeIdRef.current = id;
        await refreshTabs();
        term.writeln(`\x1b[32mGrokPtah terminal\x1b[0m — ${id.slice(0, 8)}`);

        term.onData((data) => {
          const cur = activeIdRef.current;
          if (cur) void api.ptyWrite(cur, data);
        });
        term.onResize(({ cols, rows }) => {
          const cur = activeIdRef.current;
          if (cur) void api.ptyResize(cur, cols, rows);
        });

        unlisten = await listen<PtyOutput>("pty://output", (ev) => {
          const payload = ev.payload;
          if (payload.id === activeIdRef.current) {
            term.write(payload.data);
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
      unlisten?.();
      term.dispose();
      const cur = activeIdRef.current;
      if (cur) void api.ptyKill(cur);
    };
  }, [refreshTabs]);

  // Attach agent-spawned command PTY when requested
  useEffect(() => {
    if (!attachCommand || !termRef.current) return;
    let cancelled = false;
    (async () => {
      try {
        const term = termRef.current!;
        const id = await api.ptyCreateCommand(
          attachCommand,
          term.cols,
          term.rows,
        );
        if (cancelled) {
          await api.ptyKill(id);
          return;
        }
        await switchTo(id, true);
      } catch (e) {
        termRef.current?.writeln("attach failed: " + String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [attachCommand]);

  async function switchTo(id: string, clear = true) {
    const term = termRef.current;
    if (!term) return;
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
        <span style={{ color: "var(--muted)" }}>Terminal:</span>
        {tabs.map((t) => (
          <button
            key={t}
            type="button"
            className={t === activeId ? "warn-pill" : ""}
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
