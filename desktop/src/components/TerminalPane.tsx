import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { api } from "../lib/api";

/**
 * Integrated terminal using xterm.js + in-process PTY via Tauri.
 * Output streaming is best-effort (PTY drain on backend); input/resize wired fully.
 */
export function TerminalPane() {
  const hostRef = useRef<HTMLDivElement>(null);
  const [ptyId, setPtyId] = useState<string | null>(null);
  const [tabs, setTabs] = useState<string[]>([]);
  const termRef = useRef<Terminal | null>(null);

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

    let id: string | null = null;
    let disposed = false;

    (async () => {
      try {
        const cols = term.cols;
        const rows = term.rows;
        id = await api.ptyCreate(cols, rows);
        if (disposed) {
          await api.ptyKill(id);
          return;
        }
        setPtyId(id);
        setTabs(await api.ptyList());
        term.writeln("\x1b[32mGrokPtah terminal\x1b[0m — PTY " + id.slice(0, 8));
        term.onData((data) => {
          if (id) void api.ptyWrite(id, data);
        });
        term.onResize(({ cols, rows }) => {
          if (id) void api.ptyResize(id, cols, rows);
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
      term.dispose();
      if (id) void api.ptyKill(id);
    };
  }, []);

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
        }}
      >
        <span style={{ color: "var(--muted)" }}>Terminal tabs:</span>
        {tabs.map((t) => (
          <span key={t} className={t === ptyId ? "warn-pill" : ""}>
            {t.slice(0, 8)}
          </span>
        ))}
        <button
          type="button"
          onClick={async () => {
            const id = await api.ptyCreate(80, 24);
            setTabs(await api.ptyList());
            setPtyId(id);
          }}
        >
          New tab
        </button>
      </div>
      <div className="term-host" ref={hostRef} />
    </div>
  );
}
