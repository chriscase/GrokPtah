/*
 * GrokPtah Phase 2 design prototype — screens.
 *
 * Three directions share one product model (data.js) and one component
 * vocabulary (ui.js). What differs per direction is hierarchy and chrome:
 *   d1 — Focused Lane Workbench: one calm Lane, drawers, optional Grid.
 *   d2 — Agent Operations Home: Agent roster/detail is the front door.
 *   d3 — Adaptive Expert Workspace: multi-Lane zones with explicit scope.
 */

"use strict";

/* ---------------------------------------------------------------- */
/* Small shared pieces                                               */
/* ---------------------------------------------------------------- */

function navLink(dir, screen, label, iconName, isActive, extra) {
  return (
    '<a class="rail-link' + (isActive ? " is-active" : "") + '" href="#/' + dir + "/" + screen + '"' +
    (isActive ? ' aria-current="page"' : "") + ">" + icon(iconName) + "<span>" + esc(label) + "</span>" +
    (extra || "") + "</a>"
  );
}

function laneMiniLink(dir, lane, isActive) {
  return (
    '<a class="rail-lane' + (isActive ? " is-active" : "") + '" href="#/' + dir + "/lane/" + lane.id + '"' +
    (isActive ? ' aria-current="page"' : "") + ">" +
    '<span class="chip-dot tone-' + (STATE_GRAMMAR[lane.status] || {}).tone + '" aria-hidden="true"></span>' +
    '<span class="rail-lane-title">' + esc(lane.title) + "</span>" +
    '<span class="rail-lane-sub">' + esc(lane.agentId ? getAgent(lane.agentId).name : "Ad hoc") + " · " +
    esc((STATE_GRAMMAR[lane.status] || {}).label || lane.status) + "</span></a>"
  );
}

function agentHealthLine(agent) {
  return (
    '<span class="agent-health">' + stateChip(agent.lifecycle) + stateChip(agent.health) + "</span>"
  );
}

function checkpointLine(agent) {
  if (!agent.checkpoint) {
    return '<p class="soft">' + icon("flag") + "No checkpoint yet — the first completed Run creates one.</p>";
  }
  const c = agent.checkpoint;
  return (
    '<p class="soft">' + icon("flag") + "Checkpoint <strong>" + esc(c.id) + "</strong> " +
    '<span class="chip tone-good">' + icon("check") + "<span>Verified</span></span> · " + esc(c.at) +
    " · " + esc(c.reason) + "</p>"
  );
}

/* ---------------------------------------------------------------- */
/* Agent roster (home)                                               */
/* ---------------------------------------------------------------- */

function agentCard(dir, agent) {
  const active = lanesForAgent(agent.id).filter((l) => !l.archived);
  const archived = lanesForAgent(agent.id).filter((l) => l.archived);
  const current = agent.currentLane ? getLane(agent.currentLane) : null;
  const retired = agent.lifecycle === "retired";
  return (
    '<article class="agent-card' + (retired ? " is-retired" : "") + '" aria-labelledby="ac-' + agent.id + '">' +
    '<header class="agent-card-head">' +
      '<span class="agent-avatar" aria-hidden="true">' + icon("agent") + "</span>" +
      '<div><h3 id="ac-' + agent.id + '"><a href="#/' + dir + "/agent/" + agent.id + '">' + esc(agent.name) + "</a></h3>" +
      '<p class="agent-role">' + esc(agent.role) + "</p></div>" +
      agentHealthLine(agent) +
    "</header>" +
    '<dl class="agent-facts">' +
      "<div><dt>Runtime</dt><dd>" + runtimeChip(agent.runtime, retired ? "connected" : undefined) + "</dd></div>" +
      "<div><dt>Lanes</dt><dd>" + active.length + " active · " + archived.length + " archived</dd></div>" +
      "<div><dt>Now</dt><dd>" +
        (current
          ? '<a href="#/' + dir + "/lane/" + current.id + '">' + esc(current.title) + "</a> — " +
            esc((STATE_GRAMMAR[current.status] || {}).label || current.status)
          : retired ? "Retired — no new work" : "Idle") +
      "</dd></div>" +
    "</dl>" +
    checkpointLine(agent) +
    '<footer class="agent-card-actions">' +
      '<a class="btn btn-secondary btn-sm" href="#/' + dir + "/agent/" + agent.id + '">Open details</a>' +
      (retired
        ? '<span class="soft">History preserved</span>'
        : '<button type="button" class="btn btn-ghost btn-sm">' + (agent.lifecycle === "paused" ? "Unpause" : "Pause") + "</button>" +
          '<button type="button" class="btn btn-ghost btn-sm" data-modal="retire:' + agent.id + '">Retire…</button>') +
    "</footer></article>"
  );
}

function screenAgents(dir) {
  const demo = APP.demo.agents;
  let body;
  if (demo === "loading") {
    body = stateCard("loading", { title: "Loading Agents…" });
  } else if (demo === "error") {
    /* Error REPLACES the roster and the empty state. Never both. */
    body = stateCard("error", {
      title: "Couldn’t refresh durable Agents.",
      body: "The last loaded roster may be stale. Your Agents, Lanes, and Runs are safe on disk — this is a read problem, not data loss.",
      action: "Retry refresh",
      demo: "demo-agents-loaded",
      technical:
        "orchestration store ~/.grokptah/orchestration is already open\n(Resource temporarily unavailable, os error 35)\nowner: another GrokPtah process may be running",
    });
  } else if (demo === "empty") {
    body = stateCard("empty", {
      icon: "agent",
      title: "No durable Agents yet.",
      body: "An Agent is a long-lived identity with a role, policy, and memory. Create one, or just start an ad-hoc Lane — you can attach an Agent later.",
      actions:
        '<div class="row-gap center"><button type="button" class="btn btn-primary">Create Agent</button>' +
        '<button type="button" class="btn btn-secondary">Start ad-hoc Lane</button></div>',
    });
  } else {
    const attention = AGENTS.filter((a) => a.health === "needs_attention" || a.health === "interrupted");
    body =
      (dir === "d2"
        ? '<div class="roster-summary">' +
          '<div class="sum-cell"><strong>' + AGENTS.filter((a) => a.lifecycle === "active").length + "</strong><span>Active</span></div>" +
          '<div class="sum-cell"><strong>' + AGENTS.filter((a) => a.lifecycle === "paused").length + "</strong><span>Paused</span></div>" +
          '<div class="sum-cell"><strong>' + AGENTS.filter((a) => a.lifecycle === "retired").length + "</strong><span>Retired</span></div>" +
          '<div class="sum-cell sum-attn"><strong>' + attention.length + "</strong><span>Need attention</span></div>" +
          "</div>"
        : "") +
      '<div class="agent-grid">' + AGENTS.map((a) => agentCard(dir, a)).join("") + "</div>" +
      '<section class="adhoc-section" aria-labelledby="adhoc-h">' +
      '<h3 id="adhoc-h">Ad-hoc work (no Agent)</h3>' +
      '<p class="soft">Lanes can exist without a durable Agent. Assign one at any time — assignment never rewrites the Lane’s history.</p>' +
      '<div class="lane-rows">' +
      adHocLanes().filter((l) => !l.archived).map((l) => laneRow(dir, l, { compact: true })).join("") +
      "</div></section>";
  }
  return (
    '<div class="screen">' +
    '<header class="screen-head"><div><h1 tabindex="-1">Agents</h1>' +
    '<p class="screen-sub">Durable identities. Each Agent can own many Lanes; retiring an Agent is separate from archiving its Lanes.</p></div>' +
    '<div class="row-gap"><button type="button" class="btn btn-primary">' + icon("plus") + "Create Agent</button></div></header>" +
    body +
    "</div>"
  );
}

/* ---------------------------------------------------------------- */
/* Agent detail                                                      */
/* ---------------------------------------------------------------- */

function laneGroup(dir, title, lanes, opts) {
  const o = opts || {};
  if (!lanes.length && !o.always) return "";
  const inner = lanes.length
    ? '<div class="lane-rows">' + lanes.map((l) => laneRow(dir, l, o)).join("") + "</div>"
    : '<p class="soft">' + esc(o.emptyText || "None.") + "</p>";
  if (o.collapsed) {
    return (
      '<details class="lane-group"><summary><h3>' + esc(title) + " (" + lanes.length + ")</h3></summary>" + inner + "</details>"
    );
  }
  return '<section class="lane-group"><h3>' + esc(title) + (lanes.length ? " (" + lanes.length + ")" : "") + "</h3>" + inner + "</section>";
}

function screenAgentDetail(dir, agentId) {
  const agent = getAgent(agentId);
  if (!agent) return notFound("Agent");
  const lanes = lanesForAgent(agent.id);
  const attention = lanes.filter((l) => !l.archived && ["blocked", "interrupted", "awaiting_approval", "failed"].includes(l.status));
  const active = lanes.filter((l) => !l.archived && !attention.includes(l));
  const archived = lanes.filter((l) => l.archived);
  const retired = agent.lifecycle === "retired";

  return (
    '<div class="screen">' +
    '<nav class="crumbs" aria-label="Breadcrumb"><a href="#/' + dir + '/agents">Agents</a>' + icon("chevr") +
    "<span aria-current=\"page\">" + esc(agent.name) + "</span></nav>" +
    '<header class="screen-head agent-detail-head">' +
      '<span class="agent-avatar lg" aria-hidden="true">' + icon("agent") + "</span>" +
      '<div class="agent-detail-id"><h1 tabindex="-1">' + esc(agent.name) + "</h1>" +
      '<p class="screen-sub">' + esc(agent.role) + "</p>" + agentHealthLine(agent) + "</div>" +
      '<div class="row-gap head-actions">' +
        (retired
          ? '<button type="button" class="btn btn-secondary">Unretire…</button>'
          : '<button type="button" class="btn btn-ghost">' + (agent.lifecycle === "paused" ? "Unpause" : "Pause") + "</button>" +
            '<button type="button" class="btn btn-ghost" data-modal="retire:' + agent.id + '">Retire…</button>') +
      "</div>" +
    "</header>" +

    (retired
      ? banner(
          "muted", "moon",
          "This Agent is retired.",
          "It cannot start new Lanes or Runs. Its memory, checkpoints, and every historical Lane below remain inspectable. Retiring did not archive or delete anything.",
          ""
        )
      : "") +

    '<div class="detail-cols">' +
      '<section class="card" aria-labelledby="idn-h"><h3 id="idn-h">Identity</h3>' +
        '<dl class="kv">' +
        "<div><dt>Model</dt><dd><code>" + esc(agent.model) + "</code></dd></div>" +
        "<div><dt>Policy</dt><dd>" + esc(agent.policy) + "</dd></div>" +
        "<div><dt>Memory</dt><dd>" + esc(agent.memory) + "</dd></div>" +
        "<div><dt>Runtime</dt><dd>" + runtimeChip(agent.runtime) + "</dd></div>" +
        "</dl>" + checkpointLine(agent) +
      "</section>" +
      '<section class="card" aria-labelledby="start-h"><h3 id="start-h">Start a new Lane</h3>' +
        (retired
          ? '<p class="soft">Blocked — retired Agents cannot start new work.</p>'
          : '<div class="start-lane-form">' +
            '<label for="nl-title-' + agent.id + '">Objective</label>' +
            '<input id="nl-title-' + agent.id + '" type="text" placeholder="What should this Lane accomplish?" />' +
            '<fieldset><legend>Runtime target</legend>' +
            ["local_desktop", "local_service", "hosted_service"].map((tid, i) => {
              const t = RUNTIME_TARGETS[tid];
              const s = STATE_GRAMMAR[t.connection];
              return (
                '<label class="radio-row"><input type="radio" name="nl-rt-' + agent.id + '" value="' + tid + '"' + (i === 0 ? " checked" : "") + " />" +
                icon(t.icon) + "<span>" + esc(t.name) + '</span><span class="chip tone-' + s.tone + '">' + icon(s.icon) + "<span>" + esc(s.label) + "</span></span></label>"
              );
            }).join("") +
            "</fieldset>" +
            '<p class="soft">The target is chosen before the first prompt and shown in the Lane header from then on.</p>' +
            '<button type="button" class="btn btn-primary">Start Lane</button>' +
            "</div>") +
      "</section>" +
    "</div>" +

    laneGroup(dir, "Attention needed", attention, { emptyText: "Nothing needs attention." }) +
    laneGroup(dir, "Active Lanes", active, { always: true, emptyText: "No active Lanes. Start one above." }) +
    laneGroup(dir, "Archived Lanes", archived, { collapsed: true, always: true, emptyText: "No archived Lanes." }) +

    (!retired && adHocLanes().filter((l) => !l.archived).length
      ? '<section class="lane-group"><h3>Adopt ad-hoc work</h3>' +
        '<p class="soft">These Lanes have no Agent. Assigning ' + esc(agent.name) + " is explicit and appears in the Lane header; the Lane’s history is unchanged.</p>" +
        '<div class="lane-rows">' +
        adHocLanes().filter((l) => !l.archived).map((l) =>
          laneRow(dir, l, { compact: true, extraAction: '<button type="button" class="btn btn-secondary btn-sm" data-modal="assign:' + l.id + ":" + agent.id + '">Assign to ' + esc(agent.name) + "</button>" })
        ).join("") +
        "</div></section>"
      : "") +
    "</div>"
  );
}

/* ---------------------------------------------------------------- */
/* Lane rows / list / archive / search                               */
/* ---------------------------------------------------------------- */

function laneIndicators(lane) {
  const parts = [];
  if (lane.currentRun) parts.push('<span class="ind" title="Current run">' + icon("run") + esc(lane.currentRun) + "</span>");
  if (lane.queue && lane.queue.count) parts.push('<span class="ind" title="Queued prompts">' + icon("stack") + lane.queue.count + "</span>");
  if (lane.approvals && lane.approvals.pending) parts.push('<span class="ind tone-review" title="Pending approvals">' + icon("shield") + lane.approvals.pending + "</span>");
  if (lane.changes && lane.changes.files) parts.push('<span class="ind" title="Changed files">' + icon("diff") + lane.changes.files + "</span>");
  if (lane.tests && lane.tests.status === "pass") parts.push('<span class="ind tone-good" title="Tests observed passing">' + icon("beaker") + "pass</span>");
  if (lane.tests && lane.tests.status === "not_observed") parts.push('<span class="ind tone-warn" title="Tests not observed">' + icon("beaker") + "?</span>");
  return parts.length ? '<span class="ind-row">' + parts.join("") + "</span>" : "";
}

function laneRow(dir, lane, opts) {
  const o = opts || {};
  const agent = lane.agentId ? getAgent(lane.agentId) : null;
  const searchable = (lane.title + " " + lane.objective + " " + (agent ? agent.name : "ad hoc") + " " + lane.workspace.name).toLowerCase();
  return (
    '<article class="lane-row' + (lane.archived ? " is-archived" : "") + '" data-search="' + esc(searchable) + '">' +
    '<div class="lane-row-main">' +
      '<h4><a href="#/' + dir + "/lane/" + lane.id + '">' + esc(lane.title) + "</a>" +
      (lane.workspace.scratch ? ' <span class="chip tone-muted chip-xs">' + icon("box") + "<span>Scratch</span></span>" : "") +
      "</h4>" +
      (!o.compact ? '<p class="lane-objective">' + esc(lane.objective) + "</p>" : "") +
      '<p class="lane-meta">' +
        '<span class="lane-agent">' + icon("agent") + (agent ? esc(agent.name) : '<span class="adhoc">Ad hoc</span>') + "</span>" +
        '<span class="lane-ws">' + icon("box") + esc(lane.workspace.name) + "</span>" +
        runtimeChip(lane.runtime, lane.connection) +
      "</p>" +
    "</div>" +
    '<div class="lane-row-state">' +
      stateChip(lane.status) + laneIndicators(lane) +
      '<p class="lane-next">' + esc(lane.nextAction) + "</p>" +
      '<p class="lane-when">Last activity ' + esc(lane.lastActivity) + (lane.archivedAt ? " · archived " + esc(lane.archivedAt) : "") + "</p>" +
    "</div>" +
    '<div class="lane-row-actions">' +
      '<a class="btn btn-secondary btn-sm" href="#/' + dir + "/lane/" + lane.id + '">Open</a>' +
      (o.extraAction || "") +
      (lane.archived
        ? '<button type="button" class="btn btn-ghost btn-sm">Restore</button>'
        : '<button type="button" class="btn btn-ghost btn-sm" data-modal="archive:' + lane.id + '">Archive…</button>') +
    "</div></article>"
  );
}

function screenLanes(dir, view) {
  const v = view || "active";
  let lanes, emptyText;
  if (v === "active") { lanes = activeLanes(); emptyText = "No active Lanes. Start one, or restore from the Archive."; }
  else if (v === "attention") { lanes = attentionLanes(); emptyText = "Nothing needs attention."; }
  else if (v === "archived") { lanes = archivedLanes(); emptyText = "No archived Lanes. Archiving a Lane keeps its full history and can always be undone."; }
  else { lanes = LANES.slice(); emptyText = "No Lanes yet."; }

  const tabs = [
    ["active", "Active", activeLanes().length],
    ["attention", "Attention", attentionLanes().length],
    ["archived", "Archived", archivedLanes().length],
    ["all", "All", LANES.length],
  ]
    .map(([key, label, n]) =>
      '<a class="tab' + (v === key ? " is-active" : "") + '" href="#/' + dir + "/lanes/" + key + '"' +
      (v === key ? ' aria-current="page"' : "") + ">" + esc(label) + ' <span class="tab-count">' + n + "</span></a>")
    .join("");

  return (
    '<div class="screen">' +
    '<header class="screen-head"><div><h1 tabindex="-1">Lanes</h1>' +
    '<p class="screen-sub">Work contexts. Lanes turn over quickly — archive freely; nothing is lost and restore is one click.</p></div>' +
    '<div class="row-gap"><button type="button" class="btn btn-primary">' + icon("plus") + "Start Lane</button></div></header>" +
    '<div class="lanes-toolbar">' +
      '<nav class="tabs" aria-label="Lane views">' + tabs + "</nav>" +
      '<div class="search-wrap">' + icon("search") +
      '<label class="visually-hidden" for="lane-search">Search Lanes by title, Agent, or workspace</label>' +
      '<input id="lane-search" type="search" placeholder="Search title, Agent, workspace…" />' +
      "</div>" +
    "</div>" +
    '<p class="soft search-hint" id="lane-search-status" role="status" aria-live="polite"></p>' +
    (v === "archived" && lanes.length
      ? banner("muted", "box", "Archived Lanes keep everything.", "Transcript, Runs, checkpoints, approvals, evidence, and Agent ownership are preserved. Restore returns a Lane to Active; nothing here deletes work.", "")
      : "") +
    (lanes.length
      ? '<div class="lane-rows" id="lane-list">' + lanes.map((l) => laneRow(dir, l)).join("") + "</div>"
      : stateCard("empty", { icon: "lane", title: emptyText })) +
    "</div>"
  );
}

/* ---------------------------------------------------------------- */
/* Focused Lane workspace                                            */
/* ---------------------------------------------------------------- */

function connOverrideFor(lane) {
  if (APP.demo.conn && lane.runtime !== "local_desktop" && !lane.archived) return APP.demo.conn;
  return null;
}

function laneWorkspaceCore(dir, lane, opts) {
  const o = opts || {};
  const connOverride = connOverrideFor(lane);
  return (
    contextHeader(lane, { compact: o.compact, connOverride: connOverride }) +
    laneNextAction(lane, connOverride) +
    transcript(lane) +
    composer(lane, { connOverride: connOverride, zone: o.zone })
  );
}

function drawerDock(dir, lane) {
  const open = APP.ui.drawer;
  const strip = DRAWERS.map((d) => {
    const badge =
      d.id === "approvals" && lane.approvals && lane.approvals.pending
        ? '<span class="dock-badge" aria-hidden="true">' + lane.approvals.pending + "</span>"
        : d.id === "queue" && lane.queue && lane.queue.count
        ? '<span class="dock-badge" aria-hidden="true">' + lane.queue.count + "</span>"
        : "";
    return (
      '<button type="button" class="dock-btn' + (open === d.id ? " is-open" : "") + '" data-drawer="' + d.id + '"' +
      ' aria-expanded="' + (open === d.id ? "true" : "false") + '" aria-controls="drawer-panel" title="' + esc(d.label) + '">' +
      icon(d.icon) + '<span class="dock-label">' + esc(d.label) + "</span>" + badge + "</button>"
    );
  }).join("");

  const openDef = DRAWERS.find((d) => d.id === open);
  const panel = openDef
    ? '<section class="drawer-panel" id="drawer-panel" role="region" aria-label="' + esc(openDef.label) + '">' +
      '<header class="drawer-head"><h3>' + icon(openDef.icon) + esc(openDef.label) + "</h3>" +
      '<button type="button" class="btn btn-ghost btn-sm" data-drawer-close aria-label="Close ' + esc(openDef.label) + ' drawer">Close</button></header>' +
      '<div class="drawer-body">' + drawerBody(open, lane) + "</div></section>"
    : "";

  return '<div class="dock-wrap' + (openDef ? " has-panel" : "") + '"><nav class="dock-strip" aria-label="Lane drawers">' + strip + "</nav>" + panel + "</div>";
}

function screenLane(dir, laneId) {
  const lane = getLane(laneId);
  if (!lane) return notFound("Lane");
  const agent = lane.agentId ? getAgent(lane.agentId) : null;
  const crumbs =
    dir === "d2"
      ? '<nav class="crumbs" aria-label="Breadcrumb">' +
        (agent
          ? '<a href="#/d2/agents">Agents</a>' + icon("chevr") + '<a href="#/d2/agent/' + agent.id + '">' + esc(agent.name) + "</a>"
          : '<a href="#/d2/lanes">Lanes</a>') +
        icon("chevr") + '<span aria-current="page">' + esc(lane.title) + "</span></nav>"
      : "";

  return (
    '<div class="screen screen-lane">' + crumbs +
    '<div class="lane-layout">' +
      '<div class="lane-main"><h1 class="visually-hidden" tabindex="-1">Lane: ' + esc(lane.title) + "</h1>" +
      laneWorkspaceCore(dir, lane) + "</div>" +
      drawerDock(dir, lane) +
    "</div></div>"
  );
}

/* ---------------------------------------------------------------- */
/* D1 Grid / D3 workspace: multi-lane with explicit scope            */
/* ---------------------------------------------------------------- */

function zoneCard(dir, lane, zoneName) {
  return (
    '<section class="zone" aria-label="Zone ' + esc(zoneName) + ": " + esc(lane.title) + '">' +
    '<header class="zone-head"><span class="zone-tag">' + esc(zoneName) + "</span>" +
    '<span class="zone-title">' + esc(lane.title) + "</span>" +
    '<a class="btn btn-ghost btn-sm" href="#/' + dir + "/lane/" + lane.id + '">' + icon("focus") + "Focus</a></header>" +
    laneWorkspaceCore(dir, lane, { compact: true, zone: zoneName }) +
    "</section>"
  );
}

function screenGrid(dir) {
  const a = getLane("lane-1");
  const b = getLane("lane-2");
  const pinned = getLane(APP.ui.pinned || "lane-1");
  const isD3 = dir === "d3";
  const inspectorTabs = ["queue", "changes", "approvals", "runs"];
  const openTab = inspectorTabs.includes(APP.ui.inspectorTab) ? APP.ui.inspectorTab : "changes";

  return (
    '<div class="screen screen-grid">' +
    '<header class="scope-bar" role="region" aria-label="Workspace scope">' +
      '<h1 class="scope-title" tabindex="-1">' + icon("grid") + (isD3 ? "Supervision workspace" : "Expert Grid") + "</h1>" +
      '<span class="scope-note">Viewing <strong>2 Lanes</strong>. Every zone and panel names its own Lane — nothing follows tab focus.</span>' +
      '<a class="btn btn-ghost btn-sm" href="#/' + dir + '/lane/lane-1">' + icon("focus") + "Exit to focused Lane</a>" +
    "</header>" +
    '<div class="grid-layout' + (isD3 ? " with-inspector" : "") + '">' +
      '<div class="zones">' + zoneCard(dir, a, "Zone A") + zoneCard(dir, b, "Zone B") + "</div>" +
      (isD3
        ? '<aside class="inspector" aria-label="Inspector">' +
          '<header class="inspector-head"><h3>' + icon("eye") + "Inspector</h3>" +
          '<label class="inspector-pin">Pinned to' +
          '<select id="inspector-pin-select">' +
          [a, b].map((l) => '<option value="' + l.id + '"' + (pinned.id === l.id ? " selected" : "") + ">" + esc(l.title) + "</option>").join("") +
          "</select></label></header>" +
          '<p class="drawer-scope">' + icon("lane") + '<span class="drawer-scope-text">Showing <strong>' + esc(pinned.title) + "</strong> only. Changing zone focus never changes this panel.</span></p>" +
          '<nav class="tabs tabs-sm" aria-label="Inspector sections">' +
          inspectorTabs.map((t) => {
            const def = DRAWERS.find((d) => d.id === t);
            return (
              '<button type="button" class="tab' + (openTab === t ? " is-active" : "") + '" data-inspector-tab="' + t + '"' +
              (openTab === t ? ' aria-current="true"' : "") + ">" + esc(def.label) + "</button>"
            );
          }).join("") +
          "</nav>" +
          '<div class="drawer-body">' + drawerBody(openTab, pinned) + "</div>" +
          "</aside>"
        : "") +
    "</div></div>"
  );
}

/* ---------------------------------------------------------------- */
/* Runtime targets screen                                            */
/* ---------------------------------------------------------------- */

function screenRuntime(dir) {
  const cards = ["local_desktop", "local_service", "hosted_service"]
    .map((tid) => {
      const t = RUNTIME_TARGETS[tid];
      const s = STATE_GRAMMAR[t.connection];
      return (
        '<article class="card runtime-card" aria-labelledby="rt-' + tid + '">' +
        '<header class="runtime-head">' + icon(t.icon, "lg") +
        '<div><h3 id="rt-' + tid + '">' + esc(t.name) + "</h3>" +
        '<span class="chip tone-' + s.tone + '">' + icon(s.icon) + "<span>" + esc(s.label) + "</span></span>" +
        (t.connection !== "connected" ? '<span class="soft"> · last seen ' + esc(t.lastSeen) + "</span>" : "") +
        "</div>" +
        (t.connection === "disconnected"
          ? '<button type="button" class="btn btn-primary btn-sm">Reconnect</button>'
          : "") +
        "</header>" +
        '<dl class="kv">' +
        "<div><dt>Workspaces</dt><dd>" + esc(t.workspaceAuthority) + "</dd></div>" +
        "<div><dt>What syncs</dt><dd>" + esc(t.syncPolicy) + "</dd></div>" +
        "<div><dt>Supports</dt><dd>" + t.supports.map((x) => '<span class="chip tone-neutral chip-xs"><span>' + esc(x) + "</span></span>").join(" ") + "</dd></div>" +
        "</dl>" +
        '<details class="tech"><summary>Technical details</summary><pre>' + esc(t.detail) + "</pre></details>" +
        "</article>"
      );
    })
    .join("");

  const laneMap = activeLanes()
    .map((l) => {
      const t = RUNTIME_TARGETS[l.runtime];
      const s = STATE_GRAMMAR[l.connection] || STATE_GRAMMAR.connected;
      return (
        "<tr><td><a href=\"#/" + dir + "/lane/" + l.id + '">' + esc(l.title) + "</a></td>" +
        "<td>" + esc(l.agentId ? getAgent(l.agentId).name : "Ad hoc") + "</td>" +
        "<td>" + icon(t.icon) + " " + esc(t.name) + "</td>" +
        '<td><span class="chip tone-' + s.tone + '">' + icon(s.icon) + "<span>" + esc(s.label) + "</span></span></td></tr>"
      );
    })
    .join("");

  return (
    '<div class="screen">' +
    '<header class="screen-head"><div><h1 tabindex="-1">Runtime targets</h1>' +
    '<p class="screen-sub">Where Lanes execute. Every Lane shows its target in its header, and the composer names the target before each prompt.</p></div></header>' +
    '<div class="runtime-grid">' + cards + "</div>" +
    '<section class="card"><h3>Where each active Lane runs</h3>' +
    '<div class="table-wrap"><table class="data-table"><caption class="visually-hidden">Active Lanes and their runtime targets</caption>' +
    "<thead><tr><th scope=\"col\">Lane</th><th scope=\"col\">Agent</th><th scope=\"col\">Runtime</th><th scope=\"col\">Connection</th></tr></thead>" +
    "<tbody>" + laneMap + "</tbody></table></div></section>" +
    '<p class="soft evidence-note">Hosted-service behavior shown here reflects the documented service contract (durable ledger, allowlisted workspaces, reconnect recovery). The Phase 1 audit did not exercise a hosted session end-to-end; treat hosted flows as design intent pending a product walkthrough.</p>' +
    "</div>"
  );
}

/* ---------------------------------------------------------------- */
/* Modals                                                            */
/* ---------------------------------------------------------------- */

function modalContent(kind, arg) {
  if (kind === "archive") {
    const lane = getLane(arg);
    return {
      title: "Archive Lane “" + lane.title + "”?",
      body:
        '<p>Archiving removes this Lane from Active views. It <strong>keeps everything</strong>: transcript, ' +
        "Runs, checkpoints, approvals, evidence, and its Agent relationship.</p>" +
        '<ul class="modal-list">' +
        "<li>" + icon("check") + "Reversible — Restore returns it to Active.</li>" +
        "<li>" + icon("check") + "No files are deleted; no worktree is removed.</li>" +
        "<li>" + icon("check") + (lane.agentId ? esc(getAgent(lane.agentId).name) + " is not affected." : "No Agent is affected.") + "</li>" +
        "</ul>" +
        '<p class="soft">Deleting permanently is a different action, kept away from this one.</p>',
      confirm: "Archive Lane",
      confirmClass: "btn-primary",
    };
  }
  if (kind === "retire") {
    const agent = getAgent(arg);
    const active = lanesForAgent(agent.id).filter((l) => !l.archived).length;
    return {
      title: "Retire Agent “" + agent.name + "”?",
      body:
        "<p>Retiring ends this identity’s working life. It is <strong>not</strong> the same as archiving Lanes:</p>" +
        '<ul class="modal-list">' +
        "<li>" + icon("x") + "No new Lanes or Runs can start under this Agent.</li>" +
        "<li>" + icon("check") + "Memory, checkpoints, and all " + lanesForAgent(agent.id).length + " historical Lanes stay inspectable.</li>" +
        "<li>" + icon("warn") + (active ? active + " active Lane(s) will be blocked from new work until reassigned." : "No active Lanes are affected.") + "</li>" +
        "</ul>",
      confirm: "Retire Agent",
      confirmClass: "btn-danger",
    };
  }
  if (kind === "assign") {
    const [laneId, agentId] = arg.split(":");
    const lane = getLane(laneId);
    const agent = getAgent(agentId);
    return {
      title: "Assign “" + lane.title + "” to " + agent.name + "?",
      body:
        "<p>The Lane keeps its transcript and Runs exactly as they are. From the next Run onward, " +
        esc(agent.name) + "’s policy, memory, and checkpoints apply, and the Lane header shows the Agent.</p>",
      confirm: "Assign Agent",
      confirmClass: "btn-primary",
    };
  }
  if (kind === "about") {
    return {
      title: "About this prototype",
      body:
        "<p>Static design prototype for GrokPtah issue #308 Phase 2. Three directions, one shared product model: " +
        "durable <strong>Agents</strong>, high-turnover <strong>Lanes</strong>, durable <strong>Runs</strong>, and explicit runtime targets.</p>" +
        "<p>All data is fixture data. Hosted-service and Computer Use surfaces reflect documented contracts and audited failure states only — " +
        "no successful hosted round-trip or Computer Use capture is depicted, because the Phase 1 audit did not observe one.</p>" +
        '<p class="soft">See <code>docs/ux-design/phase-2/README.md</code> for the full package.</p>',
      confirm: null,
    };
  }
  return null;
}

function notFound(what) {
  return (
    '<div class="screen">' +
    stateCard("empty", { icon: "question", title: what + " not found in fixture data", body: "Use the navigation to pick an existing item." }) +
    "</div>"
  );
}
