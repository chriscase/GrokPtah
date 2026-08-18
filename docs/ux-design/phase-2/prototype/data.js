/*
 * GrokPtah Phase 2 design prototype — shared fixture data.
 *
 * All data here is illustrative. It follows the product model in
 * docs/AGENT_LANE_RUNTIME_MODEL.md:
 *   - Agents are durable identities (lifecycle separate from health).
 *   - Lanes are high-turnover work contexts, frequently archived.
 *   - Runs are durable executions inside a Lane.
 *   - Runtime targets: local_desktop | local_service | hosted_service.
 *
 * Evidence boundary: hosted-service and Computer Use fixtures illustrate the
 * documented contracts (HEADLESS_SERVICE.md); the Phase 1 audit did NOT
 * observe a successful hosted round-trip or Computer Use capture, and this
 * prototype does not claim one. Computer Use is deliberately shown in its
 * observed "permissions not granted" state.
 *
 * Timestamps are fixed strings so captures are deterministic.
 */

"use strict";

/* ---------------------------------------------------------------- *
 * Shared state grammar
 *
 * Every user-facing state has: a label, a tone (drives color+icon),
 * a one-line meaning, and a default next action. Error and empty are
 * mutually exclusive by construction: views render exactly one state.
 * ---------------------------------------------------------------- */

const STATE_GRAMMAR = {
  // tone: neutral | active | good | info | warn | danger | muted
  loading: { label: "Loading", tone: "muted", icon: "spinner" },
  ready: { label: "Ready", tone: "neutral", icon: "dot" },
  running: { label: "Running", tone: "active", icon: "pulse" },
  waiting: { label: "Waiting", tone: "neutral", icon: "dot" },
  queued: { label: "Queued", tone: "info", icon: "stack" },
  awaiting_approval: { label: "Awaiting approval", tone: "review", icon: "shield" },
  blocked: { label: "Blocked", tone: "warn", icon: "warn" },
  interrupted: { label: "Interrupted", tone: "warn", icon: "pause" },
  disconnected: { label: "Disconnected", tone: "danger", icon: "unplug" },
  stale: { label: "Reconnecting", tone: "warn", icon: "sync" },
  failed: { label: "Failed", tone: "danger", icon: "x" },
  unverified: { label: "Unverified", tone: "warn", icon: "question" },
  completed: { label: "Completed", tone: "good", icon: "check" },
  archived: { label: "Archived", tone: "muted", icon: "box" },
  retired: { label: "Retired", tone: "muted", icon: "moon" },
  missing_workspace: { label: "Workspace missing", tone: "warn", icon: "folderx" },
  needs_attention: { label: "Needs attention", tone: "warn", icon: "warn" },
  paused: { label: "Paused", tone: "muted", icon: "pause" },
  active: { label: "Active", tone: "good", icon: "dot" },
  connected: { label: "Connected", tone: "good", icon: "link" },
  connecting: { label: "Connecting", tone: "info", icon: "sync" },
};

/* ---------------------------------------------------------------- *
 * Runtime targets
 * ---------------------------------------------------------------- */

const RUNTIME_TARGETS = {
  local_desktop: {
    id: "local_desktop",
    kind: "local_desktop",
    name: "Local desktop",
    icon: "laptop",
    connection: "connected",
    workspaceAuthority: "This desktop chooses workspace folders directly.",
    syncPolicy: "Everything stays on this Mac. Nothing is uploaded.",
    supports: ["Queue", "Steering", "Terminal", "Approvals", "Diffs & tests", "Computer Use (when granted)"],
    lastSeen: "now",
    detail: "In-process bridge in the desktop app.",
  },
  local_service: {
    id: "local_service",
    kind: "local_service",
    name: "Local service / VM",
    icon: "server",
    connection: "disconnected",
    workspaceAuthority: "The service only accepts folders on its allowlist.",
    syncPolicy:
      "Runs, transcripts, and checkpoints stay in the service's durable ledger on the VM. Credentials and source files are not synchronized.",
    supports: ["Queue", "Steering", "Approvals", "Diffs & tests", "Durable run history"],
    lastSeen: "2026-08-17 07:12",
    detail: "grokptah-service on vm-buildbox.local:39200 (loopback tunnel).",
  },
  hosted_service: {
    id: "hosted_service",
    kind: "hosted_service",
    name: "Hosted service",
    icon: "cloud",
    connection: "connected",
    workspaceAuthority: "The hosted service only accepts folders on its allowlist.",
    syncPolicy:
      "Runs, transcripts, and checkpoints live in the hosted durable ledger and are readable after reconnect. Credentials and source files are not synchronized.",
    supports: ["Queue", "Steering", "Approvals", "Diffs & tests", "Durable run history", "Reconnect recovery"],
    lastSeen: "now",
    detail: "Authenticated HTTPS; bearer token held in the desktop backend only.",
  },
};

/* ---------------------------------------------------------------- *
 * Agents — durable identities
 * lifecycle: active | paused | retired      (a decision the user made)
 * health:    ready | running | waiting | interrupted | failed
 *            | needs_attention               (what is happening right now)
 * ---------------------------------------------------------------- */

const AGENTS = [
  {
    id: "agent-1",
    name: "Ptah Refactorer",
    role: "Refactors bridge crates; keeps retry and queue semantics intact.",
    lifecycle: "active",
    health: "running",
    model: "grok-4.5",
    policy: "Workspace writes allowed · shell allowed · network denied",
    memory: "12 memory notes · scoped to crates/codegen",
    runtime: "local_desktop",
    checkpoint: { id: "c-12", verified: true, at: "2026-08-16 22:04", reason: "Terminal run completed" },
    currentLane: "lane-1",
    laneIds: ["lane-1", "lane-6"],
  },
  {
    id: "agent-2",
    name: "Release Warden",
    role: "Guards release branches; qualifies provider profiles and gateways.",
    lifecycle: "active",
    health: "needs_attention",
    model: "grok-4.5",
    policy: "Isolated worktrees only · promotion requires approval",
    memory: "31 memory notes · release rituals and gateway quirks",
    runtime: "hosted_service",
    checkpoint: { id: "c-77", verified: true, at: "2026-08-17 06:48", reason: "Run interrupted by service restart" },
    currentLane: "lane-2",
    laneIds: ["lane-2", "lane-4", "lane-9"],
  },
  {
    id: "agent-3",
    name: "Docs Curator",
    role: "Sweeps docs for drift after merged changes; proposes edits only.",
    lifecycle: "paused",
    health: "waiting",
    model: "grok-4-mini",
    policy: "Read-mostly · docs/ writes only · no shell",
    memory: "6 memory notes · terminology decisions",
    runtime: "local_service",
    checkpoint: null, // "No checkpoint yet" case
    currentLane: null,
    laneIds: ["lane-10", "lane-5"],
  },
  {
    id: "agent-4",
    name: "Legacy Migrator",
    role: "Migrated session records to the durable run ledger. Work complete.",
    lifecycle: "retired",
    health: "ready",
    model: "grok-4",
    policy: "Retired — cannot start new Lanes or Runs",
    memory: "18 memory notes · preserved, read-only",
    runtime: "local_desktop",
    checkpoint: { id: "c-41", verified: true, at: "2026-07-30 18:22", reason: "Final migration run completed" },
    currentLane: null,
    laneIds: ["lane-8"],
  },
];

/* ---------------------------------------------------------------- *
 * Lanes — high-turnover work contexts
 * status: draft | ready | running | awaiting_approval | blocked |
 *         interrupted | completed | failed | archived
 * ---------------------------------------------------------------- */

const LANES = [
  {
    id: "lane-1",
    title: "Stabilize retry queue",
    objective: "Make queued-run promotion deterministic when a capacity slot frees during shutdown.",
    agentId: "agent-1",
    workspace: { name: "grokptah / crates/codegen", path: "~/dev/grokptah", status: "ready" },
    branch: "worktree wt/retry-queue",
    runtime: "local_desktop",
    connection: "connected",
    status: "running",
    currentRun: "r-311",
    lastActivity: "2026-08-17 09:41",
    queue: { count: 2, next: "Add a soak test for the promotion race" },
    changes: { files: 4, additions: 96, deletions: 31, promotionReady: false },
    tests: { status: "pass", command: "cargo test -p grokptah-agent-bridge queue::", observed: "2026-08-17 09:38" },
    approvals: { pending: 0 },
    nextAction: "View progress or steer the active run.",
    archived: false,
  },
  {
    id: "lane-2",
    title: "Gateway model qualification",
    objective: "Qualify the two OpenAI-compatible gateway profiles against the parity checklist.",
    agentId: "agent-2",
    workspace: { name: "grokptah / desktop", path: "allowlisted: /srv/work/grokptah", status: "ready" },
    branch: "worktree wt/gateway-qualify",
    runtime: "hosted_service",
    connection: "connected",
    status: "awaiting_approval",
    currentRun: "r-402",
    lastActivity: "2026-08-17 08:55",
    queue: { count: 0, next: null },
    changes: { files: 6, additions: 210, deletions: 44, promotionReady: true },
    tests: { status: "pass", command: "npx vitest run src/lib/gateway", observed: "2026-08-17 08:52" },
    approvals: { pending: 1, runId: "r-402", fingerprint: "6 files · sha256:9f2c…e1a4" },
    nextAction: "Review the isolated diff, then approve or deny promotion.",
    archived: false,
  },
  {
    id: "lane-3",
    title: "Fix onboarding copy",
    objective: "Tighten the first-run empty-state copy in the desktop app.",
    agentId: null, // ad hoc lane
    workspace: { name: "grokptah / desktop", path: "~/dev/grokptah/desktop", status: "ready" },
    branch: "main (shared)",
    runtime: "local_desktop",
    connection: "connected",
    status: "ready",
    currentRun: null,
    lastActivity: "2026-08-16 17:20",
    queue: { count: 0, next: null },
    changes: { files: 0, additions: 0, deletions: 0, promotionReady: false },
    tests: { status: "none", command: null, observed: null },
    approvals: { pending: 0 },
    nextAction: "Send a prompt, or assign a durable Agent first.",
    archived: false,
  },
  {
    id: "lane-4",
    title: "Migrate settings schema",
    objective: "Move provider profiles to the v3 settings schema with a reversible migration.",
    agentId: "agent-2",
    workspace: { name: "grokptah / desktop", path: "allowlisted: /srv/work/grokptah", status: "ready" },
    branch: "worktree wt/settings-v3",
    runtime: "hosted_service",
    connection: "connected",
    status: "interrupted",
    currentRun: null,
    lastRun: "r-501",
    lastActivity: "2026-08-17 06:48",
    queue: { count: 0, next: null },
    changes: { files: 3, additions: 58, deletions: 12, promotionReady: false },
    tests: { status: "not_observed", command: "npx vitest run src/lib/settings", observed: null },
    approvals: { pending: 0 },
    checkpoint: { id: "c-77", verified: true, at: "2026-08-17 06:48" },
    nextAction: "Resume from verified checkpoint c-77, retry the interrupted Run, or start fresh.",
    archived: false,
  },
  {
    id: "lane-5",
    title: "Nightly docs sweep",
    objective: "Reconcile docs/ with the last five merged changes.",
    agentId: "agent-3",
    workspace: { name: "grokptah-docs (missing)", path: "~/dev/grokptah-docs", status: "missing" },
    branch: "main (shared)",
    runtime: "local_desktop",
    connection: "connected",
    status: "blocked",
    currentRun: null,
    lastActivity: "2026-08-15 23:02",
    queue: { count: 0, next: null },
    changes: { files: 0, additions: 0, deletions: 0, promotionReady: false },
    tests: { status: "none", command: null, observed: null },
    approvals: { pending: 0 },
    nextAction: "Choose a valid folder for this Lane. Nothing can run until then.",
    archived: false,
  },
  {
    id: "lane-9",
    title: "Provider profile audit",
    objective: "Cross-check request budgets on every gateway profile.",
    agentId: "agent-2",
    workspace: { name: "grokptah / desktop", path: "allowlisted: /srv/work/grokptah", status: "ready" },
    branch: "main (shared)",
    runtime: "hosted_service",
    connection: "connected",
    status: "queued",
    currentRun: "r-410",
    lastActivity: "2026-08-17 09:12",
    queue: { count: 1, position: 2, next: "Waiting for a capacity slot on the hosted service" },
    changes: { files: 0, additions: 0, deletions: 0, promotionReady: false },
    tests: { status: "none", command: null, observed: null },
    approvals: { pending: 0 },
    nextAction: "Run r-410 is queued at position 2. You can cancel or let it start.",
    archived: false,
  },
  {
    id: "lane-10",
    title: "Release notes draft",
    objective: "Draft release notes from the durable run ledger on the docs VM.",
    agentId: "agent-3",
    workspace: { name: "grokptah-docs (VM)", path: "allowlisted: /srv/work/docs", status: "ready" },
    branch: "main (shared)",
    runtime: "local_service",
    connection: "disconnected",
    status: "blocked",
    currentRun: null,
    lastActivity: "2026-08-17 07:12",
    queue: { count: 0, next: null },
    changes: { files: 0, additions: 0, deletions: 0, promotionReady: false },
    tests: { status: "none", command: null, observed: null },
    approvals: { pending: 0 },
    nextAction: "Reconnect to vm-buildbox.local, or switch this Lane to another Runtime.",
    archived: false,
  },
  /* ---- Archived lanes ---- */
  {
    id: "lane-6",
    title: "Spike: MCP filesystem trust",
    objective: "Prototype the project-trust gate for repo-local .mcp.json.",
    agentId: "agent-1",
    workspace: { name: "grokptah / crates/codegen", path: "~/dev/grokptah", status: "ready" },
    branch: "worktree wt/mcp-trust (removed)",
    runtime: "local_desktop",
    connection: "connected",
    status: "archived",
    currentRun: null,
    lastActivity: "2026-08-14 16:40",
    queue: { count: 0, next: null },
    changes: { files: 2, additions: 40, deletions: 8, promotionReady: false },
    tests: { status: "pass", command: "cargo test mcp_trust", observed: "2026-08-14 16:32" },
    approvals: { pending: 0 },
    nextAction: "Restore to continue. Transcript, Runs, and evidence are preserved.",
    archived: true,
    archivedAt: "2026-08-14 17:01",
  },
  {
    id: "lane-7",
    title: "Scratch: list-files demo",
    objective: "Throwaway demo lane used during a walkthrough.",
    agentId: null,
    // Migration hygiene: scratch path is a technical detail, not the label.
    workspace: { name: "Scratch workspace", path: "/private/var/folders/…/.tmp28GFtd", status: "ready", scratch: true },
    branch: "none",
    runtime: "local_desktop",
    connection: "connected",
    status: "archived",
    currentRun: null,
    lastActivity: "2026-08-12 11:05",
    queue: { count: 0, next: null },
    changes: { files: 0, additions: 0, deletions: 0, promotionReady: false },
    tests: { status: "none", command: null, observed: null },
    approvals: { pending: 0 },
    nextAction: "Restore to continue, or leave archived.",
    archived: true,
    archivedAt: "2026-08-12 11:20",
  },
  {
    id: "lane-8",
    title: "Parity scoreboard refresh",
    objective: "Regenerate the parity scoreboard after the ledger migration.",
    agentId: "agent-4",
    workspace: { name: "grokptah / docs", path: "~/dev/grokptah", status: "ready" },
    branch: "main (shared)",
    runtime: "local_desktop",
    connection: "connected",
    status: "archived",
    currentRun: null,
    lastActivity: "2026-07-30 18:22",
    queue: { count: 0, next: null },
    changes: { files: 1, additions: 12, deletions: 12, promotionReady: false },
    tests: { status: "none", command: null, observed: null },
    approvals: { pending: 0 },
    nextAction: "Owned by a retired Agent. History is preserved and read-only by default.",
    archived: true,
    archivedAt: "2026-07-30 18:30",
  },
];

/* ---------------------------------------------------------------- *
 * Runs — durable executions inside a Lane
 * ---------------------------------------------------------------- */

const RUNS = {
  "lane-1": [
    {
      id: "r-309", state: "completed", origin: "desktop", started: "2026-08-16 21:12", ended: "2026-08-16 22:04",
      summary: "Reproduced the promotion race with a soak harness.",
      checkpoint: "c-12", executionMode: "shared",
    },
    {
      id: "r-310", state: "completed", origin: "desktop", started: "2026-08-17 08:20", ended: "2026-08-17 09:02",
      summary: "Serialized slot release behind the admission lock; queue tests green.",
      parent: "r-309", executionMode: "isolated_worktree",
    },
    {
      id: "r-311", state: "running", origin: "desktop", started: "2026-08-17 09:31", ended: null,
      summary: "Adding regression coverage for shutdown-time promotion.",
      parent: "r-310", executionMode: "isolated_worktree",
      progress: { step: 4, of: 6, note: "Running cargo test (2 of 3 suites)" },
    },
  ],
  "lane-2": [
    {
      id: "r-401", state: "completed", origin: "mcp", started: "2026-08-17 07:58", ended: "2026-08-17 08:31",
      summary: "Qualified profile A; recorded effort values and budget behavior.",
      executionMode: "isolated_worktree",
    },
    {
      id: "r-402", state: "awaiting_approval", origin: "mcp", started: "2026-08-17 08:34", ended: "2026-08-17 08:55",
      summary: "Qualified profile B; produced a 6-file isolated diff ready for review.",
      parent: "r-401", executionMode: "isolated_worktree",
      approval: { fingerprint: "sha256:9f2c…e1a4", files: 6 },
    },
  ],
  "lane-4": [
    {
      id: "r-501", state: "interrupted", origin: "mcp", started: "2026-08-17 06:02", ended: "2026-08-17 06:48",
      summary: "Migration halted mid-write when the service restarted. Checkpoint c-77 verified.",
      executionMode: "isolated_worktree", checkpoint: "c-77",
    },
  ],
  "lane-9": [
    {
      id: "r-410", state: "queued", origin: "desktop", started: null, ended: null,
      summary: "Queued at position 2 behind one running Lane on the hosted service.",
      executionMode: "shared", queuedPosition: 2,
    },
  ],
  "lane-6": [
    {
      id: "r-220", state: "completed", origin: "desktop", started: "2026-08-14 15:50", ended: "2026-08-14 16:40",
      summary: "Trust gate prototype; tests pass; parked for the MCP epic.",
      executionMode: "shared",
    },
  ],
};

/* ---------------------------------------------------------------- *
 * Transcripts (abbreviated, per lane)
 * kind: user | agent | tool | test | event
 * ---------------------------------------------------------------- */

const TRANSCRIPTS = {
  "lane-1": [
    { kind: "user", text: "The queue promotes the wrong run when a slot frees during shutdown. Fix it and prove it with a test." },
    { kind: "agent", text: "I can reproduce this: release_slot() races the shutdown drain. Plan — (1) reproduce with a soak harness, (2) serialize slot release behind the admission lock, (3) add regression coverage." },
    { kind: "tool", text: "Read crates/codegen/grokptah-agent-bridge/src/queue.rs (412 lines)" },
    { kind: "tool", text: "Edit queue.rs — moved release into the admission critical section (+38 −11)" },
    { kind: "test", text: "cargo test -p grokptah-agent-bridge queue:: — 14 passed, 0 failed", status: "pass" },
    { kind: "agent", text: "Promotion is now deterministic under shutdown. Writing the regression test for the exact interleaving next." },
    { kind: "event", text: "Run r-311 · step 4 of 6 · running cargo test (suite 2 of 3)" },
  ],
  "lane-2": [
    { kind: "user", text: "Qualify both gateway profiles against the parity checklist. Isolated worktree, no promotion without my review." },
    { kind: "agent", text: "Profile A qualified in r-401. Profile B needed a budget clamp and an effort-value map; both are in the isolated diff." },
    { kind: "test", text: "npx vitest run src/lib/gateway — 22 passed, 0 failed", status: "pass" },
    { kind: "event", text: "Run r-402 finished in an isolated worktree. Promotion requires your approval." },
  ],
  "lane-4": [
    { kind: "user", text: "Migrate provider profiles to the v3 settings schema. Keep it reversible." },
    { kind: "agent", text: "Schema map drafted; writing the forward migration now. Reverse path verified against fixture profiles." },
    { kind: "event", text: "Run r-501 was interrupted: the hosted service restarted during a write. Checkpoint c-77 was verified before the interruption." },
  ],
  "lane-5": [
    { kind: "event", text: "This Lane's saved folder is missing. No tools or model turns can run until a valid folder is chosen." },
  ],
  "lane-3": [
    { kind: "user", text: "Rewrite the first-run empty state so it explains Builds vs Chats in one line each." },
    { kind: "agent", text: "Draft ready in my notes. Send “go” and I will apply it to OnboardingCopy.tsx." },
  ],
  "lane-9": [
    { kind: "event", text: "Run r-410 is queued at position 2 on the hosted service. It will start when a capacity slot frees." },
  ],
  "lane-10": [
    { kind: "event", text: "The local service at vm-buildbox.local has been unreachable since 07:12. Live controls are paused; durable history remains on the VM." },
  ],
  "lane-6": [
    { kind: "user", text: "Prototype the project-trust gate for repo-local .mcp.json." },
    { kind: "agent", text: "Done — gate blocks server start until trust is granted once per project. Tests cover deny-by-default." },
    { kind: "test", text: "cargo test mcp_trust — 6 passed, 0 failed", status: "pass" },
  ],
};

/* ---------------------------------------------------------------- *
 * Queue / steering fixtures
 * ---------------------------------------------------------------- */

const QUEUES = {
  "lane-1": [
    { id: "q-1", text: "Add a soak test for the promotion race", position: 1 },
    { id: "q-2", text: "Then update DURABLE_RUNS.md with the new ordering guarantee", position: 2 },
  ],
  "lane-9": [
    { id: "q-9", text: "Cross-check request budgets on every gateway profile", position: 2, service: true },
  ],
};

/* ---------------------------------------------------------------- *
 * Diff fixtures
 * ---------------------------------------------------------------- */

const DIFFS = {
  "lane-1": [
    { file: "crates/…/src/queue.rs", add: 38, del: 11 },
    { file: "crates/…/src/admission.rs", add: 22, del: 9 },
    { file: "crates/…/tests/queue_shutdown.rs", add: 31, del: 0 },
    { file: "docs/DURABLE_RUNS.md", add: 5, del: 11 },
  ],
  "lane-2": [
    { file: "desktop/src/lib/gateway/profileA.ts", add: 12, del: 4 },
    { file: "desktop/src/lib/gateway/profileB.ts", add: 64, del: 18 },
    { file: "desktop/src/lib/gateway/budget.ts", add: 41, del: 6 },
    { file: "desktop/src/lib/gateway/effort.ts", add: 38, del: 2 },
    { file: "desktop/src/lib/gateway/qualify.test.ts", add: 49, del: 10 },
    { file: "docs/PROVIDER_PROFILES.md", add: 6, del: 4 },
  ],
  "lane-4": [
    { file: "desktop/src/lib/settings/schemaV3.ts", add: 34, del: 0 },
    { file: "desktop/src/lib/settings/migrate.ts", add: 20, del: 8 },
    { file: "desktop/src/lib/settings/migrate.test.ts", add: 4, del: 4 },
  ],
};

/* ---------------------------------------------------------------- *
 * Computer Use — reflects the audited state only.
 * The Phase 1 audit observed macOS grants absent and a storage lock;
 * no successful capture is claimed anywhere in this prototype.
 * ---------------------------------------------------------------- */

const COMPUTER_USE = {
  state: "unavailable",
  message: "Computer Use is unavailable on this desktop.",
  reasons: [
    { label: "Screen Recording permission", value: "Not granted" },
    { label: "Accessibility permission", value: "Not granted" },
  ],
  action: "Grant both permissions to GrokPtah in macOS System Settings, then restart the app.",
  technical: "computer-use store ~/.grokptah/computer-use is already open (Resource temporarily unavailable, os error 35)",
};

/* ---------------------------------------------------------------- *
 * MCP fixture
 * ---------------------------------------------------------------- */

const MCP = {
  trust: "This project has not been trusted yet. Repo-local .mcp.json servers stay off until you trust the project once.",
  servers: [{ name: "filesystem", transport: "stdio", state: "disabled", note: "Enable after project trust" }],
  doctor: "npx available · config valid · no servers running",
};

/* ---------------------------------------------------------------- *
 * Direction metadata (for the switcher and docs links)
 * ---------------------------------------------------------------- */

const DIRECTIONS = {
  d1: {
    id: "d1",
    num: "1",
    name: "Focused Lane Workbench",
    short: "Workbench",
    tagline: "One calm Lane by default; everything else is a drawer.",
  },
  d2: {
    id: "d2",
    num: "2",
    name: "Agent Operations Home",
    short: "Operations",
    tagline: "Durable Agents are the front door; Lanes live under them.",
  },
  d3: {
    id: "d3",
    num: "3",
    name: "Adaptive Expert Workspace",
    short: "Expert",
    tagline: "Multi-Lane supervision with scope made explicit everywhere.",
  },
};

/* Lookup helpers (shared across views) */
function getAgent(id) { return AGENTS.find((a) => a.id === id) || null; }
function getLane(id) { return LANES.find((l) => l.id === id) || null; }
function lanesForAgent(agentId) { return LANES.filter((l) => l.agentId === agentId); }
function adHocLanes() { return LANES.filter((l) => !l.agentId); }
function activeLanes() { return LANES.filter((l) => !l.archived); }
function archivedLanes() { return LANES.filter((l) => l.archived); }
function attentionLanes() {
  return LANES.filter((l) => !l.archived && ["blocked", "interrupted", "awaiting_approval", "failed"].includes(l.status));
}
function laneRuns(laneId) { return RUNS[laneId] || []; }
