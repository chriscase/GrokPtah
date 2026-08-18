/*
 * GrokPtah Phase 2 design prototype — shared UI components.
 * Pure string-template renderers used by all three directions.
 * No dependencies. All fixture strings pass through esc().
 */

"use strict";

/* ---------------------------------------------------------------- */
/* Basics                                                            */
/* ---------------------------------------------------------------- */

function esc(s) {
  return String(s == null ? "" : s)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

/* 16px stroke icons. Decorative by default (aria-hidden); the text
 * label beside the icon carries meaning, so color is never the only
 * channel. */
const ICON_PATHS = {
  dot: '<circle cx="8" cy="8" r="3.2" fill="currentColor" stroke="none"/>',
  pulse:
    '<circle cx="8" cy="8" r="3.2" fill="currentColor" stroke="none" class="ic-pulse"/><circle cx="8" cy="8" r="6" fill="none" opacity=".45"/>',
  spinner: '<path d="M8 2a6 6 0 1 1-6 6" class="ic-spin-path"/>',
  check: '<path d="M3 8.5l3.2 3.2L13 5"/>',
  x: '<path d="M4 4l8 8M12 4l-8 8"/>',
  warn: '<path d="M8 2.2 15 13.5H1z M8 6.5v3.2 M8 11.6v.2"/>',
  pause: '<path d="M5.5 3.5v9M10.5 3.5v9"/>',
  stack: '<path d="M2.5 5.5h11M2.5 8.5h11M2.5 11.5h7"/>',
  shield: '<path d="M8 1.8l5 1.8v3.6c0 3.4-2.1 5.7-5 7-2.9-1.3-5-3.6-5-7V3.6z"/><path d="M5.8 8l1.6 1.6L10.5 6.4"/>',
  unplug: '<path d="M5 7V3.5M11 7V3.5M4 7h8v2a4 4 0 0 1-8 0z M8 13v1.5"/><path d="M2 2l12 12" class="ic-slash"/>',
  sync: '<path d="M13 8a5 5 0 0 1-9 3M3 8a5 5 0 0 1 9-3"/><path d="M12.6 2.5v2.8H9.8M3.4 13.5v-2.8h2.8"/>',
  question: '<path d="M5.8 6a2.2 2.2 0 1 1 3.4 1.9c-.8.5-1.2 1-1.2 1.9M8 12.4v.2"/>',
  box: '<path d="M2.5 5.5h11v7h-11zM2.5 5.5L4 2.8h8l1.5 2.7M6.5 8h3"/>',
  moon: '<path d="M12.8 9.6A5.4 5.4 0 0 1 6.4 3.2a5.4 5.4 0 1 0 6.4 6.4z"/>',
  folderx: '<path d="M1.8 3.5h4.4l1.3 1.6h6.7v7.4H1.8z"/><path d="M6.6 7.4l3 3m0-3l-3 3"/>',
  link: '<path d="M6.5 9.5l3-3M5 7l-1.7 1.7a2.6 2.6 0 0 0 3.7 3.7L8.7 10.7M11 9l1.7-1.7a2.6 2.6 0 0 0-3.7-3.7L7.3 5.3"/>',
  laptop: '<path d="M3 3.5h10v6.5H3zM1.5 12.5h13"/>',
  server: '<path d="M2.5 3h11v4h-11zM2.5 9h11v4h-11zM4.8 5h.2M4.8 11h.2"/>',
  cloud: '<path d="M4.5 12.5a3 3 0 0 1-.4-6A4 4 0 0 1 12 6.6a2.8 2.8 0 0 1-.6 5.9z"/>',
  agent: '<path d="M8 1.8l5.4 3.1v6.2L8 14.2l-5.4-3.1V4.9z"/><circle cx="8" cy="7" r="1.7"/><path d="M5.4 11a3 3 0 0 1 5.2 0"/>',
  lane: '<path d="M3 2.5v11M13 2.5v11M8 2.5v2m0 3v2m0 3v2"/>',
  run: '<path d="M5.5 3.5l7 4.5-7 4.5z"/>',
  flag: '<path d="M4 14V2.5M4 3h8l-2 2.7 2 2.7H4"/>',
  terminal: '<path d="M3 5l3 3-3 3M8.5 11.5H13"/>',
  diff: '<path d="M5 2.5v5M2.5 5h5M2.5 11h5M9.5 4l4 0M9.5 12l4 0M11.5 2v4M11.5 10v4" stroke-width="1.3"/>',
  beaker: '<path d="M6 2h4M6.8 2v4L3.4 12a1.4 1.4 0 0 0 1.2 2h6.8a1.4 1.4 0 0 0 1.2-2L9.2 6V2M5 10h6"/>',
  plug: '<path d="M5 2.5V6M11 2.5V6M4 6h8v2.5a4 4 0 0 1-8 0zM8 12.5V15"/>',
  monitor: '<path d="M2 3h12v8H2zM6 13.5h4M8 11v2.5"/>',
  history: '<path d="M8 4.5V8l2.5 1.5"/><path d="M2.6 8a5.4 5.4 0 1 1 1.6 3.8M2.6 8H1m1.6 0l1.2-1.6"/>',
  gear: '<circle cx="8" cy="8" r="2.2"/><path d="M8 2v1.6M8 12.4V14M2 8h1.6M12.4 8H14M3.8 3.8l1.1 1.1M11.1 11.1l1.1 1.1M12.2 3.8l-1.1 1.1M4.9 11.1l-1.1 1.1"/>',
  search: '<circle cx="7" cy="7" r="4.2"/><path d="M10.2 10.2 14 14"/>',
  plus: '<path d="M8 3v10M3 8h10"/>',
  chevr: '<path d="M6 3.5 10.5 8 6 12.5"/>',
  grid: '<path d="M2.5 2.5h4.6v4.6H2.5zM8.9 2.5h4.6v4.6H8.9zM2.5 8.9h4.6v4.6H2.5zM8.9 8.9h4.6v4.6H8.9z"/>',
  focus: '<path d="M2.5 5V2.5H5M11 2.5h2.5V5M13.5 11v2.5H11M5 13.5H2.5V11"/><circle cx="8" cy="8" r="2"/>',
  send: '<path d="M2.5 8L13.5 2.8 10.8 13.2 8.2 9.3z M13.5 2.8 8.2 9.3"/>',
  restore: '<path d="M2.6 8a5.4 5.4 0 1 1 1.6 3.8"/><path d="M2.6 4.5V8h3.5"/>',
  steer: '<circle cx="8" cy="8" r="5.5"/><circle cx="8" cy="8" r="1.6"/><path d="M8 2.5V6M8 10v3.5M2.5 8H6M10 8h3.5"/>',
  eye: '<path d="M1.5 8s2.4-4.2 6.5-4.2S14.5 8 14.5 8s-2.4 4.2-6.5 4.2S1.5 8 1.5 8z"/><circle cx="8" cy="8" r="1.8"/>',
};

function icon(name, cls) {
  const p = ICON_PATHS[name] || ICON_PATHS.dot;
  return (
    '<svg class="ic' + (cls ? " " + cls : "") + '" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true" ' +
    'fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">' + p + "</svg>"
  );
}

/* ---------------------------------------------------------------- */
/* State grammar renderers                                           */
/* ---------------------------------------------------------------- */

/* One chip per state: icon + text label, colored by tone. */
function stateChip(stateKey, textOverride) {
  const s = STATE_GRAMMAR[stateKey] || { label: stateKey, tone: "neutral", icon: "dot" };
  return (
    '<span class="chip tone-' + s.tone + '">' + icon(s.icon) +
    "<span>" + esc(textOverride || s.label) + "</span></span>"
  );
}

function runtimeChip(targetId, connection) {
  const t = RUNTIME_TARGETS[targetId];
  if (!t) return "";
  const conn = connection || t.connection;
  const s = STATE_GRAMMAR[conn] || STATE_GRAMMAR.connected;
  return (
    '<span class="chip chip-runtime tone-' + s.tone + '" title="' + esc(t.name + " — " + s.label) + '">' +
    icon(t.icon) + "<span>" + esc(t.name) + "</span><span class=\"chip-sub\">" + esc(s.label) + "</span></span>"
  );
}

/* A state card renders exactly ONE of: loading | error | empty | content.
 * This encodes the "never show contradictory empty/error states" rule. */
function stateCard(kind, opts) {
  const o = opts || {};
  if (kind === "loading") {
    return (
      '<div class="state-card" role="status">' + icon("spinner", "lg spin") +
      '<p class="state-title">' + esc(o.title || "Loading…") + "</p></div>"
    );
  }
  if (kind === "error") {
    return (
      '<div class="state-card is-error" role="alert">' + icon("warn", "lg") +
      '<p class="state-title">' + esc(o.title) + "</p>" +
      (o.body ? '<p class="state-body">' + esc(o.body) + "</p>" : "") +
      (o.action ? '<button type="button" class="btn btn-primary" data-demo="' + esc(o.demo || "") + '">' + esc(o.action) + "</button>" : "") +
      (o.technical
        ? '<details class="tech"><summary>Technical details</summary><pre>' + esc(o.technical) + "</pre>" +
          '<button type="button" class="btn btn-ghost btn-sm">Copy diagnostic record</button></details>'
        : "") +
      "</div>"
    );
  }
  if (kind === "empty") {
    return (
      '<div class="state-card">' + (o.icon ? icon(o.icon, "lg") : "") +
      '<p class="state-title">' + esc(o.title) + "</p>" +
      (o.body ? '<p class="state-body">' + esc(o.body) + "</p>" : "") +
      (o.actions || "") +
      "</div>"
    );
  }
  return "";
}

/* Banner used for lane-level conditions (stale stream, disconnected…). */
function banner(tone, iconName, title, body, actionsHtml) {
  return (
    '<div class="banner tone-' + tone + '" role="status">' + icon(iconName) +
    '<div class="banner-text"><strong>' + esc(title) + "</strong>" +
    (body ? "<span>" + esc(body) + "</span>" : "") + "</div>" +
    (actionsHtml || "") +
    "</div>"
  );
}

/* ---------------------------------------------------------------- */
/* Context ownership header — required on every contextual surface   */
/* ---------------------------------------------------------------- */

function contextHeader(lane, opts) {
  const o = opts || {};
  const agent = lane.agentId ? getAgent(lane.agentId) : null;
  const run = lane.currentRun
    ? (laneRuns(lane.id).find((r) => r.id === lane.currentRun) || null)
    : null;
  const wsState =
    lane.workspace.status === "missing" ? stateChip("missing_workspace") : stateChip("ready", "Ready");
  return (
    '<div class="ctx-header' + (o.compact ? " is-compact" : "") + '" data-lane="' + esc(lane.id) + '">' +
    '<div class="ctx-cell ctx-lane">' + icon("lane") +
      '<div><span class="ctx-label">Lane</span><span class="ctx-value">' + esc(lane.title) + "</span></div>" +
      stateChip(lane.status) + "</div>" +
    '<div class="ctx-cell">' + icon("agent") +
      '<div><span class="ctx-label">Agent</span><span class="ctx-value">' +
      (agent
        ? esc(agent.name)
        : '<span class="adhoc">Ad hoc</span>') +
      "</span></div>" +
      (agent ? stateChip(agent.lifecycle) : '<button type="button" class="btn btn-ghost btn-sm" data-demo="assign-agent">Assign…</button>') +
      "</div>" +
    '<div class="ctx-cell">' + icon(RUNTIME_TARGETS[lane.runtime].icon) +
      '<div><span class="ctx-label">Runtime</span><span class="ctx-value">' + esc(RUNTIME_TARGETS[lane.runtime].name) + "</span></div>" +
      stateChip(o.connOverride || lane.connection) + "</div>" +
    '<div class="ctx-cell">' + icon("box") +
      '<div><span class="ctx-label">Workspace</span><span class="ctx-value">' + esc(lane.workspace.name) + "</span></div>" +
      wsState + "</div>" +
    '<div class="ctx-cell">' + icon("run") +
      '<div><span class="ctx-label">Run</span><span class="ctx-value">' +
      (run ? esc(run.id) : lane.lastRun ? esc(lane.lastRun) + " (last)" : "No active run") + "</span></div>" +
      (run ? stateChip(run.state) : "") + "</div>" +
    "</div>"
  );
}

/* ---------------------------------------------------------------- */
/* Transcript + composer                                             */
/* ---------------------------------------------------------------- */

function transcript(lane) {
  const items = TRANSCRIPTS[lane.id] || [];
  if (!items.length) {
    return stateCard("empty", {
      icon: "lane",
      title: "No turns in this Lane yet",
      body: "Send the first prompt below. The transcript and every Run stay with this Lane, even after it is archived.",
    });
  }
  const rows = items
    .map((t) => {
      if (t.kind === "user") {
        return '<div class="turn turn-user"><span class="turn-who">You</span><p>' + esc(t.text) + "</p></div>";
      }
      if (t.kind === "agent") {
        return '<div class="turn turn-agent"><span class="turn-who">' +
          esc(lane.agentId ? getAgent(lane.agentId).name : "Assistant") + "</span><p>" + esc(t.text) + "</p></div>";
      }
      if (t.kind === "tool") {
        return '<div class="turn turn-tool">' + icon("terminal") + "<code>" + esc(t.text) + "</code></div>";
      }
      if (t.kind === "test") {
        return (
          '<div class="turn turn-test tone-' + (t.status === "pass" ? "good" : "danger") + '">' +
          icon("beaker") + "<code>" + esc(t.text) + "</code></div>"
        );
      }
      return '<div class="turn turn-event">' + icon("history") + "<p>" + esc(t.text) + "</p></div>";
    })
    .join("");
  return '<div class="transcript">' + rows + "</div>";
}

function composer(lane, opts) {
  const o = opts || {};
  const conn = o.connOverride || lane.connection;
  const target = RUNTIME_TARGETS[lane.runtime];
  const blocked =
    lane.archived || lane.workspace.status === "missing" || conn === "disconnected" ||
    (lane.agentId && getAgent(lane.agentId).lifecycle === "retired");
  let blockedMsg = "";
  if (lane.archived) blockedMsg = "This Lane is archived. Restore it before sending a new prompt.";
  else if (lane.workspace.status === "missing") blockedMsg = "Workspace missing — choose a valid folder before sending.";
  else if (conn === "disconnected") blockedMsg = "Runtime disconnected — reconnect, or continue this objective in another Lane on a different Runtime.";
  else if (blocked) blockedMsg = "This Agent is retired and cannot start new Runs.";

  const composerId = "composer-" + lane.id + (o.zone ? "-" + o.zone : "");
  return (
    '<div class="composer' + (blocked ? " is-blocked" : "") + '">' +
    '<div class="composer-target">' +
      '<span class="composer-target-text">' + icon("send") + "Sends to <strong>" + esc(lane.title) + "</strong> on <strong>" +
      esc(target.name) + "</strong>" +
      (conn !== "connected" ? " · " + esc((STATE_GRAMMAR[conn] || {}).label || conn) : "") +
      "</span>" +
      '<button type="button" class="btn btn-ghost btn-sm" data-modal="continue:' + esc(lane.id) + '" aria-haspopup="dialog">Continue on another Runtime…</button>' +
    "</div>" +
    (blocked
      ? '<p class="composer-blocked" role="status">' + icon("warn") + esc(blockedMsg) + "</p>"
      : "") +
    '<div class="composer-row">' +
      '<label class="visually-hidden" for="' + composerId + '">Prompt for ' + esc(lane.title) + "</label>" +
      '<textarea id="' + composerId + '" rows="2" placeholder="' +
      (blocked ? "Resolve the state above first" : "Describe the next step for this Lane…") + '"' +
      (blocked ? " disabled" : "") + "></textarea>" +
      '<button type="button" class="btn btn-primary"' + (blocked ? " disabled" : "") + ">Send</button>" +
    "</div></div>"
  );
}

/* ---------------------------------------------------------------- */
/* Drawer contents (Lane-scoped contextual surfaces)                 */
/* ---------------------------------------------------------------- */

const DRAWERS = [
  { id: "queue", label: "Queue & steering", icon: "steer" },
  { id: "changes", label: "Changes & tests", icon: "diff" },
  { id: "approvals", label: "Approvals", icon: "shield" },
  { id: "terminal", label: "Terminal", icon: "terminal" },
  { id: "mcp", label: "MCP & tools", icon: "plug" },
  { id: "computer", label: "Computer Use", icon: "monitor" },
  { id: "runs", label: "Run history", icon: "history" },
];

function drawerScopeLine(lane) {
  return (
    '<p class="drawer-scope">' + icon("lane") + '<span class="drawer-scope-text">Scoped to <strong>' + esc(lane.title) + "</strong>" +
    (lane.agentId ? " · " + esc(getAgent(lane.agentId).name) : " · Ad hoc") + "</span></p>"
  );
}

function drawerBody(drawerId, lane) {
  switch (drawerId) {
    case "queue": {
      const q = QUEUES[lane.id] || [];
      const running = lane.status === "running";
      return (
        drawerScopeLine(lane) +
        (running
          ? '<div class="panel-block"><h4>Steer the active run</h4>' +
            '<label class="visually-hidden" for="steer-' + esc(lane.id) + '">Steering note for the active run</label>' +
            '<textarea id="steer-' + esc(lane.id) + '" rows="2" placeholder="Nudge r-311 without interrupting it…"></textarea>' +
            '<div class="row-gap"><button type="button" class="btn btn-secondary btn-sm">Send steering note</button>' +
            '<button type="button" class="btn btn-danger-ghost btn-sm">Cancel run r-311…</button></div></div>'
          : "") +
        '<div class="panel-block"><h4>Queued prompts' + (q.length ? " (" + q.length + ")" : "") + "</h4>" +
        (q.length
          ? '<ol class="queue-list">' +
            q.map((item) =>
              "<li><span class=\"queue-pos\">" + item.position + "</span><p>" + esc(item.text) + "</p>" +
              '<button type="button" class="btn btn-ghost btn-sm">Remove</button></li>').join("") +
            "</ol>"
          : '<p class="soft">Nothing queued. New prompts run immediately when the Lane is idle.</p>') +
        "</div>"
      );
    }
    case "changes": {
      const files = DIFFS[lane.id] || [];
      const t = lane.tests;
      return (
        drawerScopeLine(lane) +
        '<div class="panel-block"><h4>Changed files' + (files.length ? " (" + files.length + ")" : "") + "</h4>" +
        (files.length
          ? '<ul class="diff-list">' +
            files.map((f) =>
              "<li><code>" + esc(f.file) + '</code><span class="diff-counts"><span class="add">+' + f.add +
              '</span><span class="del">−' + f.del + "</span></span></li>").join("") +
            "</ul>" +
            '<div class="row-gap"><button type="button" class="btn btn-secondary btn-sm">Open full diff</button>' +
            "</div>"
          : '<p class="soft">No changes in this Lane yet.</p>') +
        "</div>" +
        '<div class="panel-block"><h4>Tests</h4>' +
        (t && t.status === "pass"
          ? '<p class="test-line tone-good">' + icon("check") + "<code>" + esc(t.command) + "</code></p>" +
            '<p class="soft">Observed passing at ' + esc(t.observed) + ".</p>"
          : t && t.status === "not_observed"
          ? '<p class="test-line tone-warn">' + icon("question") + "No test run observed for the current changes.</p>" +
            '<p class="soft">Suggested: <code>' + esc(t.command) + "</code></p>"
          : '<p class="soft">No tests observed in this Lane.</p>') +
        "</div>"
      );
    }
    case "approvals": {
      const a = lane.approvals;
      return (
        drawerScopeLine(lane) +
        (a && a.pending
          ? '<div class="panel-block approval-card"><h4>' + icon("shield") + "Promotion approval requested</h4>" +
            "<p>Run <strong>" + esc(a.runId) + "</strong> finished in an isolated worktree and wants to promote its changes.</p>" +
            '<dl class="kv"><div><dt>Bound to</dt><dd>Run ' + esc(a.runId) + "</dd></div>" +
            "<div><dt>Fingerprint</dt><dd><code>" + esc(a.fingerprint) + "</code></dd></div>" +
            "<div><dt>Expires</dt><dd>If files change again, this approval is invalidated.</dd></div></dl>" +
            '<div class="row-gap"><button type="button" class="btn btn-secondary btn-sm">Review diff first</button>' +
            '<button type="button" class="btn btn-primary btn-sm">Approve promotion</button>' +
            '<button type="button" class="btn btn-danger-ghost btn-sm">Deny</button></div></div>'
          : '<div class="panel-block"><p class="soft">No approvals waiting in this Lane. Approvals always name the Run and file fingerprint they cover.</p></div>')
      );
    }
    case "terminal":
      return (
        drawerScopeLine(lane) +
        '<div class="panel-block term-block"><div class="term-head"><span>' + icon("terminal") +
        "Terminal — runs in <code>" + esc(lane.workspace.path) + "</code></span>" +
        '<button type="button" class="btn btn-ghost btn-sm">New tab</button></div>' +
        '<pre class="term-screen" aria-label="Terminal output">$ cargo test -p grokptah-agent-bridge queue::\n' +
        "running 14 tests … ok\n$ █</pre>" +
        '<label class="visually-hidden" for="term-in-' + esc(lane.id) + '">Terminal input</label>' +
        '<input id="term-in-' + esc(lane.id) + '" class="term-input" type="text" spellcheck="false" placeholder="Type a command — runs only in this Lane’s workspace" /></div>'
      );
    case "mcp":
      return (
        drawerScopeLine(lane) +
        '<div class="panel-block"><h4>Project trust</h4><p class="soft">' + esc(MCP.trust) + "</p>" +
        '<button type="button" class="btn btn-secondary btn-sm">Trust this project…</button></div>' +
        '<div class="panel-block"><h4>Servers</h4><ul class="plain-list">' +
        MCP.servers.map((s) =>
          "<li><code>" + esc(s.name) + "</code> " + stateChip("paused", "Disabled") +
          '<span class="soft"> · ' + esc(s.note) + "</span></li>").join("") +
        "</ul><details class=\"tech\"><summary>Doctor</summary><pre>" + esc(MCP.doctor) + "</pre></details></div>"
      );
    case "computer": {
      const tab = APP.ui.cuTab === "contract" ? "contract" : "observed";
      const tabs =
        '<nav class="tabs tabs-sm cu-tabs" aria-label="Computer Use states">' +
        '<button type="button" class="tab' + (tab === "observed" ? " is-active" : "") + '" data-cu-tab="observed"' +
        (tab === "observed" ? ' aria-current="true"' : "") + ">Observed today</button>" +
        '<button type="button" class="tab' + (tab === "contract" ? " is-active" : "") + '" data-cu-tab="contract"' +
        (tab === "contract" ? ' aria-current="true"' : "") + ">Contract illustration (#273)</button></nav>";
      if (tab === "observed") {
        return (
          drawerScopeLine(lane) + tabs +
          '<div class="panel-block">' +
          stateCard("error", {
            title: COMPUTER_USE.message,
            body: COMPUTER_USE.action,
            technical:
              COMPUTER_USE.reasons.map((r) => r.label + ": " + r.value).join("\n") + "\n" + COMPUTER_USE.technical,
          }) +
          '<p class="soft evidence-note">The Phase 1 audit observed this unavailable state; a successful capture has not been observed and is not depicted.</p>' +
          "</div>"
        );
      }
      /* Contract illustration for issue #273 / rubric S14 — local only. */
      const cu = COMPUTER_USE_CONTRACT;
      return (
        drawerScopeLine(lane) + tabs +
        '<div class="panel-block cu-contract">' +
        '<p class="chip tone-review cu-contract-tag">' + icon("shield") +
          "<span>Design contract — #273 / S14 · local only · not an observed capture</span></p>" +
        '<div class="cu-target" role="group" aria-label="Computer Run identity and target">' +
          '<dl class="kv">' +
          "<div><dt>Lane · Run</dt><dd><strong>" + esc(getLane(cu.laneId).title) + "</strong> · Computer Run <strong>" +
            esc(cu.computerRun) + "</strong> (attributed to " + esc(cu.attributedRun) + ")</dd></div>" +
          "<div><dt>Target</dt><dd>" + esc(cu.target) + "</dd></div>" +
          "<div><dt>Grant</dt><dd>" + esc(cu.grantScope) + " · " + esc(cu.grantExpiry) + "</dd></div>" +
          "<div><dt>Model</dt><dd>" + esc(cu.model) + " · origin: " + esc(cu.origin) + "</dd></div>" +
          "<div><dt>Budgets</dt><dd>Actions " + cu.actionBudget.used + " of " + cu.actionBudget.max +
            " · Time " + esc(cu.timeBudget.used) + " of " + esc(cu.timeBudget.max) + "</dd></div>" +
          "<div><dt>Now</dt><dd>" + esc(cu.currentAction) + "</dd></div>" +
          '<div><dt>Observation</dt><dd><span class="chip tone-good">' + icon("check") + "<span>" + esc(cu.freshness) + "</span></span></dd></div>" +
          "</dl></div>" +
        '<div class="approval-card cu-approval"><h4>' + icon("shield") + "Action needs review</h4>" +
          "<p><strong>" + esc(cu.pendingApproval.action) + "</strong></p>" +
          '<dl class="kv"><div><dt>Bound to</dt><dd>' + esc(cu.pendingApproval.boundTo) + "</dd></div>" +
          "<div><dt>Evidence</dt><dd>" + esc(cu.pendingApproval.evidence) + "</dd></div></dl>" +
          '<div class="row-gap"><button type="button" class="btn btn-secondary btn-sm">Review evidence</button>' +
          '<button type="button" class="btn btn-primary btn-sm">Approve action</button>' +
          '<button type="button" class="btn btn-danger-ghost btn-sm">Deny</button></div></div>' +
        '<p class="soft cu-invalidation">' + icon("warn") + esc(cu.invalidation) + "</p>" +
        "</div>" +
        '<div class="cu-controls" role="group" aria-label="Operator controls — always reachable">' +
          '<button type="button" class="btn btn-secondary btn-sm">Pause</button>' +
          '<button type="button" class="btn btn-secondary btn-sm">Stop</button>' +
          '<button type="button" class="btn btn-danger btn-sm">Take over</button>' +
          '<button type="button" class="btn btn-ghost btn-sm" title="Steering guides the run without cancelling it">Steer (non-cancelling)</button>' +
        "</div>"
      );
    }
    case "runs": {
      const runs = laneRuns(lane.id);
      return (
        drawerScopeLine(lane) +
        (runs.length
          ? '<ol class="run-list">' +
            runs
              .slice()
              .reverse()
              .map((r) => {
                return (
                  '<li class="run-item"><div class="run-head"><strong>' + esc(r.id) + "</strong>" + stateChip(r.state) +
                  '<span class="soft">' + esc(r.origin) + " · " + esc(r.executionMode.replace("_", " ")) + "</span></div>" +
                  "<p>" + esc(r.summary) + "</p>" +
                  (r.parent ? '<p class="lineage">' + icon("chevr") + "Continues " + esc(r.parent) + "</p>" : "") +
                  (r.checkpoint
                    ? '<p class="lineage">' + icon("flag") + "Checkpoint <strong>" + esc(r.checkpoint) +
                      '</strong> <span class="chip tone-good">' + icon("check") + "<span>Verified</span></span> " +
                      '<button type="button" class="btn btn-ghost btn-sm">Inspect</button></p>'
                    : "") +
                  (r.progress
                    ? '<p class="progress-line">' +
                      '<progress max="' + r.progress.of + '" value="' + r.progress.step + '" aria-label="Run progress: step ' +
                      r.progress.step + " of " + r.progress.of + '"></progress> Step ' + r.progress.step + " of " + r.progress.of +
                      " — " + esc(r.progress.note) + "</p>"
                    : "") +
                  "</li>"
                );
              })
              .join("") +
            "</ol>"
          : stateCard("empty", { icon: "history", title: "No Runs in this Lane yet", body: "Every execution becomes a durable Run you can inspect later." }))
      );
    }
  }
  return "";
}

/* ---------------------------------------------------------------- */
/* Lane primary-state strip: the single clear next action            */
/* ---------------------------------------------------------------- */

function laneNextAction(lane, connOverride) {
  const conn = connOverride || lane.connection;

  if (lane.archived) {
    return banner(
      "muted", "box",
      "This Lane is archived. Its transcript, Runs, checkpoints, approvals, and evidence are preserved.",
      "Archive never deletes work. Restore the Lane to continue in it.",
      '<div class="banner-actions"><button type="button" class="btn btn-primary btn-sm">Restore Lane</button>' +
      '<button type="button" class="btn btn-ghost btn-sm">Inspect history</button></div>'
    );
  }
  if (lane.workspace.status === "missing") {
    return banner(
      "warn", "folderx",
      "This Lane’s workspace is unavailable.",
      "The saved folder cannot be found. No tools or model turns can run until you choose a valid folder. The Lane and its transcript are preserved.",
      '<div class="banner-actions"><button type="button" class="btn btn-primary btn-sm">Choose folder…</button>' +
      '<details class="tech tech-inline"><summary>Technical details</summary><pre>saved path: ' + esc(lane.workspace.path) + "\nstatus: not found</pre></details></div>"
    );
  }
  if (conn === "disconnected") {
    const t = RUNTIME_TARGETS[lane.runtime];
    return banner(
      "danger", "unplug",
      esc(t.name) + " is disconnected. Live controls are paused.",
      "Durable history stays on the service and will be readable after reconnect. Last seen " + t.lastSeen + ". This Lane stays on " + t.name + " — disconnecting never relabels it.",
      '<div class="banner-actions"><button type="button" class="btn btn-primary btn-sm">Reconnect</button>' +
      '<button type="button" class="btn btn-ghost btn-sm" data-modal="continue:' + esc(lane.id) + '">Continue on another Runtime…</button></div>'
    );
  }
  if (conn === "stale") {
    return banner(
      "warn", "sync",
      "Event stream is reconnecting.",
      "Shown state is from the last durable cursor and may be behind. Nothing is lost; the stream resumes from the retained journal.",
      '<div class="banner-actions"><button type="button" class="btn btn-secondary btn-sm">Refresh now</button></div>'
    );
  }
  if (lane.status === "interrupted") {
    return banner(
      "warn", "pause",
      "Work stopped before finishing. A verified checkpoint is available.",
      "Run " + (lane.lastRun || "") + " was interrupted. Resuming starts a new Run that continues from checkpoint " +
        (lane.checkpoint ? lane.checkpoint.id : "") + "; retrying replaces the interrupted Run.",
      '<div class="banner-actions"><button type="button" class="btn btn-primary btn-sm">Resume from checkpoint ' +
        esc(lane.checkpoint ? lane.checkpoint.id : "") + "</button>" +
      '<button type="button" class="btn btn-secondary btn-sm">Retry run</button>' +
      '<button type="button" class="btn btn-ghost btn-sm">Inspect checkpoint</button></div>'
    );
  }
  if (lane.status === "awaiting_approval") {
    return banner(
      "review", "shield",
      "Review required before changes are applied.",
      "Run " + lane.approvals.runId + " produced an isolated diff (" + lane.approvals.fingerprint + "). Nothing touches your branch until you approve.",
      '<div class="banner-actions"><button type="button" class="btn btn-primary btn-sm" data-demo="open-drawer-approvals">Review &amp; decide</button></div>'
    );
  }
  if (lane.status === "queued") {
    return banner(
      "info", "stack",
      "Run " + (lane.currentRun || "") + " is queued at position " + (lane.queue.position || "—") + ".",
      lane.queue.next || "",
      '<div class="banner-actions"><button type="button" class="btn btn-ghost btn-sm">Cancel queued run</button></div>'
    );
  }
  if (lane.agentId) {
    const agent = getAgent(lane.agentId);
    if (agent.lifecycle === "retired") {
      return banner(
        "muted", "moon",
        agent.name + " is retired (proposed lifecycle). New work in this Lane is blocked.",
        "Every Run, checkpoint, transcript, and memory record is preserved exactly as it is — retirement archives, deletes, moves, and rewrites nothing. Agent-to-Agent reassignment is not a routine action in this phase.",
        ""
      );
    }
  }
  return "";
}
