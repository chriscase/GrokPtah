import { useCallback, useEffect, useState } from "react";
import { api } from "../lib/api";
import type { AuthState, ModelInfo } from "../lib/protocol";
import { StyledSelect } from "./StyledSelect";
import {
  applyAppearanceChrome,
  loadAppearanceChrome,
  saveAppearanceChrome,
  type AppearanceChrome,
  type AccentId,
  type DensityId,
  type TypeScaleId,
} from "../lib/appearance";

export type SettingsPanelProps = {
  open: boolean;
  onClose: () => void;
  models: ModelInfo[];
  auth: AuthState;
  onAuthChange: (a: AuthState) => void;
  /** After host chrome changes (model/effort/etc). */
  onChromeChange: () => void;
  /**
   * #132: `dock` = full-height side pane (default); `modal` = centered overlay.
   */
  placement?: "dock" | "modal";
};

type SettingsSnap = {
  model?: string;
  effort?: string;
  alwaysApprove?: boolean;
  sandboxProfile?: string;
  appearance?: string;
  permissionMode?: string;
  allowRules?: string[];
  denyRules?: string[];
  autoUpdateEnabled?: boolean;
};

const EFFORTS = [
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
] as const;

/**
 * Full-screen settings: defaults (model, effort, permissions, tool safety,
 * appearance) + auth. Keeps chrome out of the titlebar/composer clutter.
 */
export function SettingsPanel({
  open,
  onClose,
  models,
  auth,
  onAuthChange,
  onChromeChange,
  placement = "dock",
}: SettingsPanelProps) {
  const [snap, setSnap] = useState<SettingsSnap>({});
  const [apiKeyInput, setApiKeyInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [section, setSection] = useState<
    "defaults" | "permissions" | "appearance" | "auth" | "about"
  >("defaults");

  const refresh = useCallback(async () => {
    try {
      const s = (await api.settingsSnapshot()) as SettingsSnap;
      setSnap(s);
    } catch (e) {
      setNotice(String(e));
    }
  }, []);

  useEffect(() => {
    if (!open) return;
    void refresh();
    setNotice(null);
  }, [open, refresh]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  async function apply<T>(fn: () => Promise<T>, okMsg?: string) {
    setBusy(true);
    setNotice(null);
    try {
      await fn();
      await refresh();
      onChromeChange();
      if (okMsg) setNotice(okMsg);
    } catch (e) {
      setNotice(String(e));
    } finally {
      setBusy(false);
    }
  }

  const modelValue =
    models.some((m) => m.id === snap.model)
      ? (snap.model ?? models[0]?.id ?? "grok-build")
      : (models[0]?.id ?? snap.model ?? "grok-build");

  return (
    <div
      className={`settings-backdrop ${placement === "dock" ? "is-dock" : "is-modal"}`}
      onClick={placement === "modal" ? onClose : undefined}
      role="presentation"
    >
      <div
        className={`settings-panel ${placement === "dock" ? "is-docked" : ""}`}
        role="dialog"
        aria-modal={placement === "modal"}
        aria-label="Settings"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="settings-header">
          <div className="settings-title">
            <span className="settings-title-icon" aria-hidden>
              <SettingsGlyph />
            </span>
            Settings
            {placement === "dock" && (
              <span className="settings-dock-badge">pane</span>
            )}
          </div>
          <button
            type="button"
            className="settings-close"
            aria-label="Close settings"
            onClick={onClose}
          >
            ×
          </button>
        </header>

        <div className="settings-body">
          <nav className="settings-nav" aria-label="Settings sections">
            {(
              [
                ["defaults", "Defaults"],
                ["permissions", "Permissions"],
                ["appearance", "Appearance"],
                ["auth", "Auth"],
                ["about", "About"],
              ] as const
            ).map(([id, label]) => (
              <button
                key={id}
                type="button"
                className={`settings-nav-item ${section === id ? "active" : ""}`}
                onClick={() => setSection(id)}
              >
                {label}
              </button>
            ))}
          </nav>

          <div className="settings-content">
            {notice && <div className="settings-notice">{notice}</div>}

            {section === "defaults" && (
              <section className="settings-section">
                <h2>Session defaults</h2>
                <p className="settings-lead">
                  Applied to new builds and chats, and used by the composer
                  unless you override there for a single turn.
                </p>

                <label className="settings-field">
                  <span className="settings-field-label">Default model</span>
                  <StyledSelect
                    disabled={busy}
                    value={modelValue}
                    options={models.map((m) => ({
                      value: m.id,
                      label: m.display_name,
                    }))}
                    onChange={(v) =>
                      void apply(() => api.setModel(v), "Default model saved")
                    }
                  />
                  <span className="settings-hint">
                    Wire id: <code>{modelValue}</code>
                  </span>
                </label>

                <label className="settings-field">
                  <span className="settings-field-label">Default effort</span>
                  <StyledSelect
                    disabled={busy}
                    value={String(snap.effort ?? "medium")}
                    options={EFFORTS.map((e) => ({ value: e, label: e }))}
                    onChange={(v) =>
                      void apply(() => api.setEffort(v), "Default effort saved")
                    }
                  />
                  <span className="settings-hint">
                    Reasoning budget preference for agent turns.
                  </span>
                </label>

              </section>
            )}

            {section === "permissions" && (
              <section className="settings-section">
                <h2>Permissions & tool safety</h2>
                <p className="settings-lead">
                  How tools run and what they may touch on disk. Tool prompting
                  is a <strong>single</strong> control (also the composer Auto
                  chip) — no second YOLO toggle that can disagree. Soft
                  agent-side gates only — not an OS sandbox.
                </p>

                <label className="settings-field">
                  <span className="settings-field-label">
                    Tool permission mode
                  </span>
                  <StyledSelect
                    disabled={busy}
                    value={
                      snap.alwaysApprove ||
                      snap.permissionMode === "bypassPermissions"
                        ? "bypassPermissions"
                        : "default"
                    }
                    options={[
                      { value: "default", label: "Prompt for each tool" },
                      {
                        value: "bypassPermissions",
                        label: "Always approve (bypass / YOLO)",
                      },
                    ]}
                    onChange={(v) =>
                      void apply(
                        () => api.setPermissionMode(v),
                        v === "bypassPermissions"
                          ? "Tools: auto-approve (bypass)"
                          : "Tools: prompt each call",
                      )
                    }
                  />
                  <span className="settings-hint">
                    Effective:{" "}
                    {snap.alwaysApprove ||
                    snap.permissionMode === "bypassPermissions"
                      ? "bypass — no tool prompts"
                      : "prompt — ask before shell/write/MCP"}
                    . Per-tool “Always allow” from the permission modal is
                    separate and only scopes that tool.
                  </span>
                </label>

                <label className="settings-field">
                  <span className="settings-field-label">
                    Tool safety profile
                  </span>
                  <StyledSelect
                    disabled={busy}
                    value={String(snap.sandboxProfile ?? "workspace-write")}
                    options={[
                      {
                        value: "workspace-write",
                        label: "workspace-write (agent soft gates)",
                      },
                      {
                        value: "read-only",
                        label: "read-only (block write tools / mutators)",
                      },
                      {
                        value: "danger-full-access",
                        label: "unrestricted (no agent-side gates)",
                      },
                    ]}
                    onChange={(v) =>
                      void apply(
                        () => api.setSandbox(v),
                        "Tool safety profile saved",
                      )
                    }
                  />
                  <span className="settings-hint">
                    Not an OS sandbox. These are substring/tool-level soft
                    rails inside the agent bridge — trivially bypassable by a
                    determined command. Do not treat this as isolation.
                  </span>
                </label>

                <div className="settings-field">
                  <span className="settings-field-label">Allow / deny rules</span>
                  <p className="settings-lead" style={{ marginTop: 0 }}>
                    Enforced at the tool gate: <strong>deny wins</strong>. Allow
                    skips the prompt for matching tools; deny blocks them.
                    Patterns: tool name, family alias (
                    <code>Shell(*)</code>, <code>WebFetch(*)</code>), or{" "}
                    <code>*</code>.
                  </p>
                  <div className="settings-rules">
                    <div>
                      <strong>Allow</strong>
                      <pre>
                        {(snap.allowRules ?? []).join("\n") || "(none)"}
                      </pre>
                    </div>
                    <div>
                      <strong>Deny</strong>
                      <pre>
                        {(snap.denyRules ?? []).join("\n") || "(none)"}
                      </pre>
                    </div>
                  </div>
                  <div className="modal-actions" style={{ marginTop: "0.5rem" }}>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() =>
                        void apply(
                          () => api.setAllowDenyRules([], []),
                          "Rules cleared",
                        )
                      }
                    >
                      Clear rules
                    </button>
                  </div>
                </div>
              </section>
            )}

            {section === "appearance" && (
              <AppearanceSection
                busy={busy}
                theme={String(snap.appearance ?? "dark")}
                apply={apply}
              />
            )}

            {section === "auth" && (
              <section className="settings-section">
                <h2>Authentication</h2>
                <p className="settings-lead">
                  Prefer <code>grok login</code> (reads{" "}
                  <code>~/.grok/auth.json</code>). Optional API key is stored
                  in the OS keychain.
                </p>

                <div className="settings-auth-card">
                  <div className="settings-auth-status">
                    <span
                      className={`settings-auth-dot ${auth.signed_in ? "on" : ""}`}
                    />
                    {auth.signed_in
                      ? `${auth.display_name || "Signed in"} · ${auth.method}`
                      : "Not signed in"}
                  </div>
                  <div className="settings-auth-actions">
                    <button
                      type="button"
                      onClick={() => void api.authOpenLogin()}
                    >
                      Open console.x.ai
                    </button>
                    {auth.signed_in && (
                      <button
                        type="button"
                        className="danger"
                        onClick={() =>
                          void apply(async () => {
                            onAuthChange(await api.signOut());
                          }, "Signed out")
                        }
                      >
                        Sign out / clear key
                      </button>
                    )}
                  </div>
                </div>

                <label className="settings-field">
                  <span className="settings-field-label">
                    API key (optional)
                  </span>
                  <div className="settings-key-row">
                    <input
                      type="password"
                      placeholder="xai-…"
                      value={apiKeyInput}
                      onChange={(e) => setApiKeyInput(e.target.value)}
                      autoComplete="off"
                    />
                    <button
                      type="button"
                      className="primary"
                      disabled={busy || !apiKeyInput.trim()}
                      onClick={() =>
                        void apply(async () => {
                          const a = await api.authSetApiKey(
                            apiKeyInput.trim(),
                            "API key",
                          );
                          onAuthChange(a);
                          setApiKeyInput("");
                        }, "API key saved")
                      }
                    >
                      Save key
                    </button>
                  </div>
                </label>
              </section>
            )}

            {section === "about" && (
              <section className="settings-section">
                <h2>About</h2>
                <p className="settings-lead">
                  GrokPtah desktop — Grok Build as a local coding agent.
                </p>
                <ul className="settings-about-list">
                  <li>
                    Config home: <code>~/.grokptah/</code>
                  </li>
                  <li>
                    CLI auth: <code>~/.grok/auth.json</code>
                  </li>
                  <li>
                    Auto-update:{" "}
                    {snap.autoUpdateEnabled === false
                      ? "disabled (fork)"
                      : "n/a"}
                  </li>
                </ul>
              </section>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

/** #134 — theme + accent/density/type-scale with live preview. */
function AppearanceSection({
  busy,
  theme,
  apply,
}: {
  busy: boolean;
  theme: string;
  apply: <T,>(fn: () => Promise<T>, msg: string) => Promise<void>;
}) {
  const [chrome, setChrome] = useState<AppearanceChrome>(() =>
    loadAppearanceChrome(),
  );

  useEffect(() => {
    applyAppearanceChrome(chrome);
  }, [chrome]);

  function patchChrome(partial: Partial<AppearanceChrome>) {
    const next = { ...chrome, ...partial };
    setChrome(next);
    saveAppearanceChrome(next);
  }

  return (
    <section className="settings-section">
      <h2>Appearance</h2>
      <label className="settings-field">
        <span className="settings-field-label">Theme</span>
        <StyledSelect
          disabled={busy}
          value={theme}
          options={[
            { value: "dark", label: "Dark" },
            { value: "light", label: "Light" },
          ]}
          onChange={(v) =>
            void apply(async () => {
              await api.setAppearance(v);
              document.documentElement.dataset.theme = v;
            }, "Theme saved")
          }
        />
      </label>
      <label className="settings-field">
        <span className="settings-field-label">Accent</span>
        <StyledSelect
          disabled={busy}
          value={chrome.accent}
          options={[
            { value: "amber", label: "Amber (default)" },
            { value: "teal", label: "Teal" },
            { value: "violet", label: "Violet" },
          ]}
          onChange={(v) => patchChrome({ accent: v as AccentId })}
        />
      </label>
      <label className="settings-field">
        <span className="settings-field-label">Density</span>
        <StyledSelect
          disabled={busy}
          value={chrome.density}
          options={[
            { value: "compact", label: "Compact" },
            { value: "comfortable", label: "Comfortable" },
            { value: "spacious", label: "Spacious" },
          ]}
          onChange={(v) => patchChrome({ density: v as DensityId })}
        />
      </label>
      <label className="settings-field">
        <span className="settings-field-label">Type scale</span>
        <StyledSelect
          disabled={busy}
          value={chrome.typeScale}
          options={[
            { value: "sm", label: "Small" },
            { value: "md", label: "Medium" },
            { value: "lg", label: "Large" },
          ]}
          onChange={(v) => patchChrome({ typeScale: v as TypeScaleId })}
        />
      </label>
      <div className="appearance-preview" data-testid="appearance-preview">
        <div style={{ fontSize: 11, color: "var(--muted)", marginBottom: 6 }}>
          Live preview
        </div>
        <div className="appearance-preview-bubble">
          Accent · density · type — updates as you change controls
        </div>
      </div>
    </section>
  );
}

function SettingsGlyph() {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden>
      <path
        d="M6.5 2.5h3l.4 1.6a4.5 4.5 0 0 1 1.1.6l1.6-.5.9 1.5-1.2 1.1c.1.3.1.7.1 1s0 .7-.1 1l1.2 1.1-.9 1.5-1.6-.5a4.5 4.5 0 0 1-1.1.6L9.5 13.5h-3l-.4-1.6a4.5 4.5 0 0 1-1.1-.6l-1.6.5-.9-1.5 1.2-1.1A4 4 0 0 1 3.6 8c0-.3 0-.7.1-1L2.5 5.9l.9-1.5 1.6.5c.3-.3.7-.5 1.1-.6L6.5 2.5Z"
        stroke="currentColor"
        strokeWidth="1.2"
        strokeLinejoin="round"
      />
      <circle cx="8" cy="8" r="1.75" stroke="currentColor" strokeWidth="1.2" />
    </svg>
  );
}
