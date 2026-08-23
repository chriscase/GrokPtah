import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import type {
  AuthState,
  ComputerObservationPreview,
  ComputerPermissionStatus,
  ComputerPlatformStatus,
  ComputerTargetCandidate,
  ModelInfo,
  ProviderProfileSummary,
  ProviderQualificationReport,
} from "../lib/protocol";
import { ProviderReadinessCenter } from "./ProviderReadinessCenter";
import { StyledSelect } from "./StyledSelect";
import { effortForModel, effortOptionsForModel } from "../lib/modelOptions";
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
  subagentIsolation?: "worktree" | "shared";
  subagentIsolationConfigured?: "worktree" | "shared";
  subagentIsolationManagedByEnv?: boolean;
  appearance?: string;
  permissionMode?: string;
  allowRules?: string[];
  denyRules?: string[];
  autoUpdateEnabled?: boolean;
  gatewayProviderId?: string;
  gatewayBaseUrl?: string;
  gatewayApiKeySet?: boolean;
  gatewayProfiles?: ProviderProfileSummary[];
  gatewayMigrationPending?: boolean;
};

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
  const [gatewayProvider, setGatewayProvider] = useState("");
  const [gatewayLabel, setGatewayLabel] = useState("");
  const [gatewayBase, setGatewayBase] = useState("");
  const [gatewayModel, setGatewayModel] = useState("");
  const [gatewayDeadline, setGatewayDeadline] = useState<
    "interactive" | "standard" | "extended"
  >("standard");
  const [gatewayEffort, setGatewayEffort] = useState("");
  const [gatewayKey, setGatewayKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [liveQualification, setLiveQualification] =
    useState<ProviderQualificationReport | null>(null);
  const [qualificationError, setQualificationError] = useState<string | null>(
    null,
  );
  const gatewayKeyRef = useRef<HTMLInputElement>(null);
  const qualifyInFlight = useRef(false);
  const readinessSelection = useRef(0);
  const readinessReportSelection = useRef(-1);
  const [computerStatus, setComputerStatus] =
    useState<ComputerPlatformStatus | null>(null);
  const [computerTargets, setComputerTargets] = useState<
    ComputerTargetCandidate[]
  >([]);
  const [computerPreview, setComputerPreview] =
    useState<ComputerObservationPreview | null>(null);
  const computerRequestEpoch = useRef(0);
  const [section, setSection] = useState<
    "defaults" | "permissions" | "computer" | "appearance" | "auth" | "about"
  >("defaults");

  const refresh = useCallback(async () => {
    try {
      const s = (await api.settingsSnapshot()) as SettingsSnap;
      setSnap(s);
      const profiles = s.gatewayProfiles ?? [];
      const providerId = String(
        s.gatewayProviderId ?? profiles[0]?.id ?? "",
      );
      const profile = profiles.find((item) => item.id === providerId);
      const activeModel = models.find(
        (model) => model.id === s.model && model.provider_id === providerId,
      );
      setGatewayProvider(providerId);
      setGatewayLabel(profile?.label ?? providerId);
      setGatewayBase(profile?.baseUrl ?? String(s.gatewayBaseUrl ?? ""));
      setGatewayModel(
        activeModel?.wire_model_id ?? profile?.models[0]?.id ?? "",
      );
      setGatewayEffort(
        profile?.models
          .find(
            (model) =>
              model.id ===
              (activeModel?.wire_model_id ?? profile?.models[0]?.id),
          )
          ?.effortOptions.join(", ") ?? "",
      );
      setGatewayDeadline(profile?.deadlineClass ?? "standard");
    } catch (e) {
      setNotice(String(e));
    }
  }, [models]);

  useEffect(() => {
    if (!open) return;
    void refresh();
    setNotice(null);
  }, [open, refresh]);

  useEffect(() => {
    if (!open) return;
    setLiveQualification(null);
    setQualificationError(null);
    readinessSelection.current += 1;
  }, [open]);

  const refreshComputerUse = useCallback(async () => {
    setComputerStatus(await api.computerUseStatus());
  }, []);

  useEffect(() => {
    if (!open || section !== "computer") return;
    void refreshComputerUse().catch((error) => setNotice(String(error)));
  }, [open, refreshComputerUse, section]);

  useEffect(() => {
    if (open && section === "computer") return;
    computerRequestEpoch.current += 1;
    setComputerTargets([]);
    setComputerPreview(null);
  }, [open, section]);

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

  async function applyComputer<T>(
    fn: (isCurrent: () => boolean) => Promise<T>,
    okMsg?: string,
  ) {
    const requestEpoch = computerRequestEpoch.current;
    const isCurrent = () => computerRequestEpoch.current === requestEpoch;
    setBusy(true);
    setNotice(null);
    try {
      const result = await fn(isCurrent);
      if (!isCurrent()) return undefined;
      const status = await api.computerUseStatus();
      if (!isCurrent()) return undefined;
      setComputerStatus(status);
      if (okMsg) setNotice(okMsg);
      return result;
    } catch (error) {
      if (isCurrent()) setNotice(String(error));
      return undefined;
    } finally {
      setBusy(false);
    }
  }

  const modelValue =
    models.some((m) => m.id === snap.model)
      ? (snap.model ?? models[0]?.id ?? "grok-build")
      : (models[0]?.id ?? snap.model ?? "grok-build");
  const effortOptions = effortOptionsForModel(models, modelValue);
  const currentEffort = effortForModel(models, modelValue, snap.effort);
  const gatewayProfiles = snap.gatewayProfiles ?? [];
  const selectedGateway = gatewayProfiles.find(
    (profile) => profile.id === gatewayProvider,
  );
  const activeQualification =
    readinessReportSelection.current === readinessSelection.current
      ? liveQualification
      : null;
  const providerWriteDisabled =
    busy ||
    Boolean(selectedGateway?.managedByEnv) ||
    !gatewayProvider.trim() ||
    !gatewayBase.trim() ||
    !gatewayModel.trim();

  function discardLiveReadiness() {
    readinessSelection.current += 1;
    setLiveQualification(null);
    setQualificationError(null);
  }

  function applyGatewayModel(value: string) {
    if (value !== gatewayModel) discardLiveReadiness();
    setGatewayModel(value);
    const model = selectedGateway?.models.find((item) => item.id === value);
    setGatewayEffort(model?.effortOptions.join(", ") ?? "");
  }

  function selectGatewayProfile(providerId: string) {
    discardLiveReadiness();
    if (providerId === "__new__") {
      setGatewayProvider("");
      setGatewayLabel("");
      setGatewayBase("");
      setGatewayModel("");
      setGatewayEffort("");
      setGatewayDeadline("standard");
      setGatewayKey("");
      return;
    }
    const profile = gatewayProfiles.find((item) => item.id === providerId);
    setGatewayProvider(providerId);
    setGatewayLabel(profile?.label ?? providerId);
    setGatewayBase(profile?.baseUrl ?? "");
    setGatewayModel(profile?.models[0]?.id ?? "");
    setGatewayEffort(profile?.models[0]?.effortOptions.join(", ") ?? "");
    setGatewayDeadline(profile?.deadlineClass ?? "standard");
    setGatewayKey("");
  }

  async function saveGatewayProfile(discover: boolean) {
    const providerId = gatewayProvider.trim();
    const label = gatewayLabel.trim() || providerId;
    const baseUrl = gatewayBase.trim();
    const modelId = gatewayModel;
    await apply(async () => {
      await api.upsertProviderProfile(
        providerId,
        label,
        baseUrl,
        modelId,
        gatewayDeadline,
        gatewayEffort
          .split(",")
          .map((value) => value.trim().toLowerCase())
          .filter(Boolean),
        gatewayKey.trim() || null,
      );
      setGatewayKey("");
      if (discover) await api.discoverProviderModels(providerId);
    }, discover ? "Provider saved and models discovered" : "Provider saved");
  }

  async function qualifyGatewayModel() {
    if (qualifyInFlight.current) return;
    const providerId = gatewayProvider.trim();
    const modelId = gatewayModel;
    const selectionAtStart = readinessSelection.current;
    qualifyInFlight.current = true;
    setBusy(true);
    setNotice(null);
    setQualificationError(null);
    try {
      await api.upsertProviderProfile(
        providerId,
        gatewayLabel.trim() || providerId,
        gatewayBase.trim(),
        modelId,
        gatewayDeadline,
        gatewayEffort
          .split(",")
          .map((value) => value.trim().toLowerCase())
          .filter(Boolean),
        gatewayKey.trim() || null,
      );
      setGatewayKey("");
      const report = await api.qualifyProviderModel(providerId, modelId);
      if (readinessSelection.current !== selectionAtStart) return;
      setLiveQualification(report);
      readinessReportSelection.current = selectionAtStart;
      await refresh();
      onChromeChange();
    } catch (error) {
      if (readinessSelection.current !== selectionAtStart) return;
      setQualificationError(String(error));
    } finally {
      qualifyInFlight.current = false;
      setBusy(false);
    }
  }

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
                ["computer", "Computer Use"],
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
                      void apply(async () => {
                        await api.setModel(v);
                        const nextEffort = effortForModel(models, v, snap.effort);
                        if (nextEffort !== snap.effort) {
                          await api.setEffort(nextEffort);
                        }
                      }, "Default model saved")
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
                    value={currentEffort}
                    options={effortOptions.map((e) => ({ value: e, label: e }))}
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

                <label className="settings-field">
                  <span className="settings-field-label">
                    Mutating subagent folders
                  </span>
                  <StyledSelect
                    disabled={busy || snap.subagentIsolationManagedByEnv}
                    value={snap.subagentIsolation ?? "worktree"}
                    options={[
                      {
                        value: "worktree",
                        label: "Separate worktree / copy (recommended)",
                      },
                      {
                        value: "shared",
                        label: "Shared project folder (unsafe)",
                      },
                    ]}
                    onChange={(value) =>
                      void apply(
                        () =>
                          api.setSubagentIsolation(
                            value as "worktree" | "shared",
                          ),
                        value === "shared"
                          ? "Shared-folder mutation enabled"
                          : "Separate subagent folders enabled",
                      )
                    }
                  />
                  <span
                    className={`settings-hint ${
                      snap.subagentIsolation === "shared" ? "is-warning" : ""
                    }`}
                  >
                    {snap.subagentIsolationManagedByEnv
                      ? `Managed by GROKPTAH_SUBAGENT_ISOLATION. Saved preference: ${
                          snap.subagentIsolationConfigured ?? "worktree"
                        }.`
                      : snap.subagentIsolation === "shared"
                        ? "Warning: mutating children can edit the same files as the parent and each other."
                        : "General-purpose children get separate worktrees. Plan and explore children stay shared and read-only."}
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

            {section === "computer" && (
              <section className="settings-section computer-use-settings">
                <h2>Computer Use</h2>
                <p className="settings-lead">
                  Read-only macOS observation. Captures require a fresh local
                  window selection; keyboard and pointer control are disabled.
                </p>

                {computerStatus ? (
                  <div className="computer-permission-list">
                    {(
                      [
                        [
                          "Screen Recording",
                          "screen_recording",
                          computerStatus.screenRecording,
                        ],
                        [
                          "Accessibility",
                          "accessibility",
                          computerStatus.accessibility,
                        ],
                      ] as const
                    ).map(([label, permission, status]) => (
                      <div className="computer-permission-row" key={permission}>
                        <div>
                          <strong>{label}</strong>
                          <span>{computerPermissionLabel(status)}</span>
                        </div>
                        {status !== "granted" && status !== "unsupported" && (
                          <button
                            type="button"
                            disabled={busy || !computerStatus.available}
                            onClick={() =>
                              void applyComputer(async () => {
                                setComputerTargets([]);
                                setComputerPreview(null);
                                await api.computerUseRequestPermission(
                                  permission,
                                );
                              }, `${label} request opened`)
                            }
                          >
                            Request
                          </button>
                        )}
                      </div>
                    ))}
                  </div>
                ) : (
                  <div className="settings-hint">Checking availability...</div>
                )}

                {computerStatus &&
                  (computerStatus.screenRecording !== "granted" ||
                    computerStatus.accessibility !== "granted") && (
                    <p className="settings-hint is-warning">
                      macOS grants are per application. Codex Computer Use and Terminal grants do not
                      grant GrokPtah access; enable both for GrokPtah in Privacy &amp; Security, then
                      restart GrokPtah and refresh.
                    </p>
                  )}

                {computerStatus?.detail && (
                  <div className="settings-hint is-warning">
                    {computerStatus.detail}
                  </div>
                )}

                <div className="computer-use-actions">
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void applyComputer(() => Promise.resolve())}
                  >
                    Refresh status
                  </button>
                  <button
                    type="button"
                    disabled={
                      busy ||
                      computerStatus?.screenRecording !== "granted" ||
                      computerStatus?.accessibility !== "granted"
                    }
                    onClick={() =>
                      void applyComputer(async (isCurrent) => {
                        setComputerPreview(null);
                        const targets = await api.computerUseListTargets();
                        if (isCurrent()) setComputerTargets(targets);
                      }, "Window list refreshed")
                    }
                  >
                    Find windows
                  </button>
                </div>

                {computerTargets.length > 0 && (
                  <div
                    className="computer-target-list"
                    aria-label="Available windows"
                  >
                    {computerTargets.map((candidate, index) => (
                      <div
                        className="computer-target-row"
                        key={candidate.selectionToken}
                      >
                        <div>
                          <strong>{candidate.target.displayName}</strong>
                          <span>
                            Window {index + 1} ·{" "}
                            {Math.round(candidate.geometry.width)} ×{" "}
                            {Math.round(candidate.geometry.height)}
                            {candidate.active ? " · active" : ""}
                            {candidate.minimized ? " · minimized" : ""}
                          </span>
                        </div>
                        <button
                          type="button"
                          disabled={
                            busy || candidate.minimized || !candidate.onScreen
                          }
                          onClick={() =>
                            void applyComputer(async (isCurrent) => {
                              const preview = await api.computerUseObserveOnce(
                                candidate.selectionToken,
                              );
                              if (isCurrent()) setComputerPreview(preview);
                            }, "Read-only observation complete")
                          }
                        >
                          Observe once
                        </button>
                      </div>
                    ))}
                  </div>
                )}

                {computerPreview && (
                  <div className="computer-preview" aria-live="polite">
                    <div className="computer-preview-meta">
                      <strong>
                        {computerPreview.observation.target.displayName}
                      </strong>
                      <span>
                        {computerPreview.observation.elements.length} accessible
                        elements
                      </span>
                    </div>
                    {computerPreview.imageDataUrl ? (
                      <img
                        src={computerPreview.imageDataUrl}
                        alt={`Redacted observation of ${computerPreview.observation.target.displayName}`}
                      />
                    ) : (
                      <div className="computer-preview-withheld">
                        Image withheld because privacy redaction could not be
                        fully verified.
                      </div>
                    )}
                  </div>
                )}
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

                <div
                  className="settings-field"
                  data-testid="settings-gateway"
                  style={{ marginTop: "1.25rem" }}
                >
                  <span className="settings-field-label">
                    Model providers
                  </span>
                  <p className="settings-lead" style={{ marginTop: 0 }}>
                    Connect OpenAI-compatible gateways without changing xAI
                    login. Each profile has its own key and exact model IDs;
                    keys are stored in the OS keychain.
                  </p>

                  {snap.gatewayMigrationPending && (
                    <div className="settings-inline-warning" role="status">
                      A legacy gateway key still needs migration. Save its
                      profile with a replacement key before adding another.
                    </div>
                  )}

                  <label className="settings-field">
                    <span className="settings-field-label">Provider profile</span>
                    <StyledSelect
                      disabled={busy}
                      value={selectedGateway ? selectedGateway.id : "__new__"}
                      options={[
                        ...gatewayProfiles.map((profile) => ({
                          value: profile.id,
                          label: `${profile.label}${profile.managedByEnv ? " (environment)" : ""}`,
                        })),
                        { value: "__new__", label: "Add provider profile" },
                      ]}
                      onChange={selectGatewayProfile}
                    />
                  </label>

                  <label className="settings-field">
                    <span className="settings-field-label">Profile ID</span>
                    <input
                      data-testid="gateway-provider-id"
                      type="text"
                      placeholder="company-gateway"
                      value={gatewayProvider}
                      disabled={busy || Boolean(selectedGateway)}
                      onChange={(e) => setGatewayProvider(e.target.value)}
                    />
                    <span className="settings-hint">
                      Stable local name; lowercase letters, numbers, dots,
                      dashes, and underscores.
                    </span>
                  </label>
                  <label className="settings-field">
                    <span className="settings-field-label">Display name</span>
                    <input
                      data-testid="gateway-label"
                      type="text"
                      placeholder="Company AI gateway"
                      value={gatewayLabel}
                      disabled={busy || selectedGateway?.managedByEnv}
                      onChange={(e) => setGatewayLabel(e.target.value)}
                    />
                  </label>
                  <label className="settings-field">
                    <span className="settings-field-label">Base URL</span>
                    <input
                      data-testid="gateway-base-url"
                      type="url"
                      placeholder="https://gateway.example/v1"
                      value={gatewayBase}
                      disabled={busy || selectedGateway?.managedByEnv}
                      onChange={(e) => setGatewayBase(e.target.value)}
                    />
                  </label>
                  <label className="settings-field">
                    <span className="settings-field-label">Model ID</span>
                    <input
                      data-testid="gateway-model-id"
                      type="text"
                      placeholder="model-id-from-your-gateway"
                      value={gatewayModel}
                      disabled={busy || selectedGateway?.managedByEnv}
                      onChange={(e) => applyGatewayModel(e.target.value)}
                    />
                    <span className="settings-hint">
                      Enter an exact ID, or save one ID and use Discover models.
                    </span>
                  </label>
                  <label className="settings-field">
                    <span className="settings-field-label">
                      Supported effort values (optional)
                    </span>
                    <input
                      data-testid="gateway-effort-options"
                      type="text"
                      placeholder="low, medium, high"
                      value={gatewayEffort}
                      disabled={busy || selectedGateway?.managedByEnv}
                      onChange={(event) => setGatewayEffort(event.target.value)}
                    />
                    <span className="settings-hint">
                      Declare only values documented by this exact gateway and
                      model. The default omits effort entirely.
                    </span>
                  </label>
                  <label className="settings-field">
                    <span className="settings-field-label">Request budget</span>
                    <StyledSelect
                      disabled={busy || selectedGateway?.managedByEnv}
                      value={gatewayDeadline}
                      options={[
                        { value: "interactive", label: "Interactive" },
                        { value: "standard", label: "Standard" },
                        { value: "extended", label: "Extended" },
                      ]}
                      onChange={(value) =>
                        setGatewayDeadline(
                          value as "interactive" | "standard" | "extended",
                        )
                      }
                    />
                    <span className="settings-hint">
                      Bounded timeout class for this gateway; slower local or
                      queued models may need Extended.
                    </span>
                  </label>
                  <label className="settings-field">
                    <span className="settings-field-label">
                      Provider API key
                      {selectedGateway?.credentialSet ? " (saved)" : ""}
                    </span>
                    <input
                      ref={gatewayKeyRef}
                      data-testid="gateway-api-key"
                      type="password"
                      placeholder={
                        selectedGateway?.credentialSet
                          ? "Saved (leave blank to keep)"
                          : "Provider key"
                      }
                      value={gatewayKey}
                      disabled={busy || selectedGateway?.managedByEnv}
                      onChange={(e) => setGatewayKey(e.target.value)}
                      autoComplete="off"
                    />
                  </label>

                  {selectedGateway && selectedGateway.models.length > 0 && (
                    <div className="settings-provider-models">
                      {selectedGateway.models.map((model) => (
                        <button
                          key={model.id}
                          type="button"
                          className={gatewayModel === model.id ? "active" : ""}
                          onClick={() => applyGatewayModel(model.id)}
                          disabled={busy}
                        >
                          <span>{model.displayName}</span>
                          <small>
                            {model.capabilitySource === "unknown"
                              ? "Not qualified"
                              : model.capabilitySource === "declared"
                                ? "Declared — not measured"
                                : [
                                    model.supportsTools ? "Tools" : "Chat",
                                    model.supportsStream ? "Streaming" : null,
                                    model.supportsImageInput ? "Images" : null,
                                    model.computerCapabilitySource ===
                                    "measured"
                                      ? model.computerUseTier ===
                                        "semantic_act"
                                        ? "Computer: semantic"
                                        : model.computerUseTier === "observe"
                                          ? "Computer: observe"
                                          : model.computerUseTier ===
                                              "visual_fallback_act"
                                            ? "Computer: visual fallback"
                                            : null
                                      : model.computerCapabilitySource ===
                                          "declared"
                                        ? "Computer declared"
                                        : null,
                                    model.effortOptions.length > 0
                                      ? `Effort: ${model.effortOptions.join(", ")}`
                                      : null,
                                  ]
                                    .filter(Boolean)
                                    .join(" · ")}
                          </small>
                        </button>
                      ))}
                    </div>
                  )}

                  <ProviderReadinessCenter
                    providerId={gatewayProvider}
                    providerLabel={gatewayLabel}
                    baseUrl={gatewayBase}
                    modelId={gatewayModel}
                    profile={selectedGateway ?? null}
                    report={activeQualification}
                    busy={busy}
                    error={qualificationError}
                    saveDisabled={providerWriteDisabled}
                    discoverDisabled={providerWriteDisabled}
                    qualifyDisabled={providerWriteDisabled}
                    onSaveProvider={() => void saveGatewayProfile(false)}
                    onDiscoverModels={() => void saveGatewayProfile(true)}
                    onQualifyModel={() => void qualifyGatewayModel()}
                    onResolveCredentials={() => {
                      gatewayKeyRef.current?.focus();
                    }}
                  />

                  {/* The readiness center above carries the single primary
                      "next safe action"; this row keeps every provider action
                      discoverable without competing primary emphasis. */}
                  <span
                    className="settings-field-label"
                    id="settings-provider-actions-label"
                  >
                    All provider actions
                  </span>
                  <div
                    className="modal-actions settings-provider-actions"
                    role="group"
                    aria-labelledby="settings-provider-actions-label"
                  >
                    <button
                      type="button"
                      data-testid="gateway-save"
                      disabled={providerWriteDisabled}
                      onClick={() => void saveGatewayProfile(false)}
                    >
                      Save provider
                    </button>
                    <button
                      type="button"
                      data-testid="gateway-discover"
                      disabled={providerWriteDisabled}
                      onClick={() => void saveGatewayProfile(true)}
                    >
                      Discover models
                    </button>
                    <button
                      type="button"
                      data-testid="gateway-qualify"
                      disabled={providerWriteDisabled}
                      onClick={() => void qualifyGatewayModel()}
                    >
                      Qualify model
                    </button>
                    {selectedGateway && !selectedGateway.managedByEnv && (
                      <button
                        type="button"
                        className="danger"
                        data-testid="gateway-delete"
                        disabled={busy}
                        onClick={() => {
                          discardLiveReadiness();
                          void apply(
                            () => api.deleteProviderProfile(selectedGateway.id),
                            "Provider removed",
                          );
                        }}
                      >
                        Remove
                      </button>
                    )}
                  </div>
                  {selectedGateway?.managedByEnv && (
                    <span className="settings-hint">
                      This profile is managed by environment variables. Change
                      those variables to edit or remove it.
                    </span>
                  )}
                </div>
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

function computerPermissionLabel(status: ComputerPermissionStatus): string {
  switch (status) {
    case "granted":
      return "Granted";
    case "prompt_pending":
      return "Waiting for macOS";
    case "revoked":
      return "Revoked";
    case "denied":
      return "Denied";
    case "restricted":
      return "Restricted";
    case "unsupported":
      return "Unavailable";
    default:
      return "Not granted";
  }
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
