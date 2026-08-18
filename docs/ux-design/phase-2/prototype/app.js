/*
 * GrokPtah Phase 2 design prototype — shell, router, interactions.
 * Routes: #/<direction>/<screen>[/<id>]
 *   d1: lane/<id> (default lane-1), lanes/<view>, agents, agent/<id>, grid, runtime
 *   d2: agents (default), agent/<id>, lanes/<view>, lane/<id>, runtime
 *   d3: workspace (default), lane/<id>, lanes/<view>, agents, agent/<id>, runtime
 */

"use strict";

const APP = {
  demo: { agents: "loaded", conn: "" }, // prototype-state controls
  ui: { drawer: null, pinned: "lane-1", inspectorTab: "changes", cuTab: "observed" },
};

const DEFAULT_ROUTE = { d1: "/d1/lane/lane-1", d2: "/d2/agents", d3: "/d3/workspace" };

/* ---------------- routing ---------------- */

function parseHash() {
  const raw = (location.hash || "").replace(/^#/, "");
  const [path, query] = raw.split("?");
  /* Demo states are deep-linkable so every error/recovery state has a
   * stable URL: #/d2/agents?demo=error, #/d1/lane/lane-2?conn=stale */
  if (query) {
    const params = new URLSearchParams(query);
    if (params.has("demo")) APP.demo.agents = params.get("demo");
    if (params.has("conn")) APP.demo.conn = params.get("conn");
    if (params.has("drawer")) APP.ui.drawer = params.get("drawer");
    if (params.has("cu")) APP.ui.cuTab = params.get("cu"); // observed | contract
  }
  const parts = path.split("/").filter(Boolean);
  const dir = DIRECTIONS[parts[0]] ? parts[0] : "d1";
  return { dir: dir, screen: parts[1] || "", id: parts[2] || "", extra: parts[3] || "" };
}

function sameScreenIn(otherDir, route) {
  // Preserve the current screen when switching direction, where it exists.
  const s = route.screen;
  if (!s) return DEFAULT_ROUTE[otherDir];
  if (s === "grid" || s === "workspace") {
    return otherDir === "d1" ? "/d1/grid" : otherDir === "d3" ? "/d3/workspace" : "/d2/agents";
  }
  const path = "/" + otherDir + "/" + s + (route.id ? "/" + route.id : "");
  return path;
}

/* ---------------- direction shells ---------------- */

function topBar(route) {
  const switcher = Object.values(DIRECTIONS)
    .map((d) => {
      const active = d.id === route.dir;
      return (
        '<a class="dir-btn' + (active ? " is-active" : "") + '" href="#' + sameScreenIn(d.id, route) + '"' +
        (active ? ' aria-current="true"' : "") + ' title="Direction ' + d.num + ": " + esc(d.name) + '">' +
        '<span class="dir-num">' + d.num + '</span><span class="dir-name">' + esc(d.short) + "</span></a>"
      );
    })
    .join("");
  return (
    '<header class="app-bar">' +
    '<a class="brand" href="#' + DEFAULT_ROUTE[route.dir] + '">' +
      '<span class="brand-mark" aria-hidden="true">' + icon("agent") + "</span>" +
      '<span class="brand-name">GrokPtah</span><span class="brand-tag">Phase 2 concept</span></a>' +
    '<nav class="dir-switch" aria-label="Design direction">' +
      '<span class="dir-switch-label">Direction</span>' + switcher + "</nav>" +
    '<span class="dir-tagline">' + esc(DIRECTIONS[route.dir].tagline) + "</span>" +
    '<div class="app-bar-actions">' +
      '<button type="button" class="btn btn-ghost btn-sm" data-open-demo aria-haspopup="dialog">' + icon("gear") + "Demo states</button>" +
      '<button type="button" class="btn btn-ghost btn-sm" data-modal="about:x" aria-haspopup="dialog">About</button>' +
    "</div></header>"
  );
}

function railD1(route) {
  const openLanes = activeLanes().slice(0, 5);
  return (
    '<nav class="rail" aria-label="Primary">' +
    '<section class="rail-section"><h2 class="rail-h">Work</h2>' +
    openLanes.map((l) => laneMiniLink("d1", l, route.screen === "lane" && route.id === l.id)).join("") +
    '<a class="rail-link rail-link-soft" href="#/d1/lanes/active">' + icon("lane") + "<span>All Lanes…</span></a>" +
    "</section>" +
    '<section class="rail-section"><h2 class="rail-h">Library</h2>' +
    navLink("d1", "agents", "Agents", "agent", route.screen === "agents" || route.screen === "agent") +
    navLink("d1", "lanes/archived", "Archive", "box", route.screen === "lanes" && route.id === "archived") +
    navLink("d1", "runtime", "Runtime", "cloud", route.screen === "runtime") +
    "</section>" +
    '<section class="rail-section rail-bottom">' +
    navLink("d1", "grid", "Expert Grid", "grid", route.screen === "grid",
      '<span class="rail-note">opt-in</span>') +
    "</section></nav>"
  );
}

function railD2(route) {
  return (
    '<nav class="rail" aria-label="Primary">' +
    '<section class="rail-section">' +
    navLink("d2", "agents", "Agents", "agent", route.screen === "agents" || route.screen === "agent",
      '<span class="rail-count">' + AGENTS.length + "</span>") +
    navLink("d2", "lanes/active", "Lanes", "lane", route.screen === "lanes" || route.screen === "lane",
      '<span class="rail-count">' + activeLanes().length + "</span>") +
    navLink("d2", "runtime", "Runtime", "cloud", route.screen === "runtime") +
    "</section>" +
    '<section class="rail-section"><h2 class="rail-h">Attention</h2>' +
    attentionLanes().map((l) => laneMiniLink("d2", l, route.screen === "lane" && route.id === l.id)).join("") +
    "</section></nav>"
  );
}

function railD3(route) {
  return (
    '<nav class="rail rail-dense" aria-label="Primary">' +
    '<section class="rail-section">' +
    navLink("d3", "workspace", "Workspace", "grid", route.screen === "workspace") +
    navLink("d3", "runtime", "Runtime", "cloud", route.screen === "runtime") +
    "</section>" +
    '<section class="rail-section"><h2 class="rail-h">Agents</h2>' +
    AGENTS.map((a) => {
      const active = route.screen === "agent" && route.id === a.id;
      return (
        '<a class="rail-lane' + (active ? " is-active" : "") + '" href="#/d3/agent/' + a.id + '"' +
        (active ? ' aria-current="page"' : "") + ">" +
        '<span class="chip-dot tone-' + (STATE_GRAMMAR[a.health] || {}).tone + '" aria-hidden="true"></span>' +
        '<span class="rail-lane-title">' + esc(a.name) + "</span>" +
        '<span class="rail-lane-sub">' + esc((STATE_GRAMMAR[a.lifecycle] || {}).label) + " · " + esc((STATE_GRAMMAR[a.health] || {}).label) + "</span></a>"
      );
    }).join("") +
    navLink("d3", "agents", "All Agents…", "agent", route.screen === "agents") +
    "</section>" +
    '<section class="rail-section"><h2 class="rail-h">Lanes</h2>' +
    activeLanes().map((l) => laneMiniLink("d3", l, route.screen === "lane" && route.id === l.id)).join("") +
    '<a class="rail-link rail-link-soft" href="#/d3/lanes/archived">' + icon("box") + "<span>Archive…</span></a>" +
    "</section></nav>"
  );
}

/* ---------------- render ---------------- */

function renderScreen(route) {
  const dir = route.dir;
  switch (route.screen) {
    case "agents": return screenAgents(dir);
    case "agent": return screenAgentDetail(dir, route.id);
    case "lanes": return screenLanes(dir, route.id);
    case "lane": return screenLane(dir, route.id);
    case "grid": return screenGrid(dir);
    case "workspace": return screenGrid(dir);
    case "runtime": return screenRuntime(dir);
    default:
      location.replace("#" + DEFAULT_ROUTE[dir]);
      return "";
  }
}

function render() {
  const route = parseHash();
  if (!route.screen) {
    location.replace("#" + DEFAULT_ROUTE[route.dir]);
    return;
  }
  const rail = route.dir === "d1" ? railD1(route) : route.dir === "d2" ? railD2(route) : railD3(route);
  const root = document.getElementById("app");
  root.innerHTML =
    '<a class="skip-link" href="#main">Skip to content</a>' +
    topBar(route) +
    '<div class="frame frame-' + route.dir + '">' + rail +
    '<main id="main" class="content" tabindex="-1">' + renderScreen(route) + "</main></div>" +
    '<p class="proto-foot">Design prototype · fixture data only · hosted and Computer Use flows reflect documented contracts and audited failure states, not observed successes.</p>';
  wireSearch();
  document.title = "GrokPtah Phase 2 — " + DIRECTIONS[route.dir].name;
}

/* Move focus to the screen heading on navigation (screen-reader sanity). */
function focusHeading() {
  const h = document.querySelector("main h1[tabindex='-1']");
  if (h) h.focus({ preventScroll: false });
}

/* ---------------- interactions ---------------- */

function wireSearch() {
  const input = document.getElementById("lane-search");
  if (!input) return;
  const status = document.getElementById("lane-search-status");
  input.addEventListener("input", function () {
    const q = input.value.trim().toLowerCase();
    const rows = document.querySelectorAll("#lane-list .lane-row");
    let shown = 0;
    rows.forEach(function (row) {
      const hit = !q || (row.getAttribute("data-search") || "").indexOf(q) !== -1;
      row.hidden = !hit;
      if (hit) shown++;
    });
    status.textContent = q
      ? shown
        ? shown + " Lane" + (shown === 1 ? "" : "s") + " match “" + q + "” — results keep Lane identity: title, Agent, workspace, and state."
        : "No Lanes match “" + q + "”. Archived Lanes are searched from the Archived view."
      : "";
  });
}

function openModal(kind, arg) {
  const def = modalContent(kind, arg);
  if (!def) return;
  const dlg = document.getElementById("modal");
  dlg.innerHTML =
    '<form method="dialog" class="modal-inner">' +
    '<h2 id="modal-title">' + esc(def.title) + "</h2>" +
    '<div class="modal-body">' + def.body + "</div>" +
    '<div class="modal-actions">' +
    '<button value="cancel" class="btn btn-ghost">' + (def.confirm ? "Cancel" : "Close") + "</button>" +
    (def.confirm ? '<button value="confirm" class="btn ' + def.confirmClass + '">' + esc(def.confirm) + "</button>" : "") +
    "</div></form>";
  dlg.showModal();
}

function openDemoDialog() {
  const dlg = document.getElementById("modal");
  function radio(group, value, label, checked) {
    return (
      '<label class="radio-row"><input type="radio" name="' + group + '" value="' + value + '"' +
      (checked ? " checked" : "") + " /><span>" + label + "</span></label>"
    );
  }
  dlg.innerHTML =
    '<form method="dialog" class="modal-inner">' +
    '<h2 id="modal-title">Demo states</h2>' +
    '<div class="modal-body">' +
    '<p class="soft">Prototype-only controls to preview the shared state grammar. Each view renders exactly one state — error and empty are never shown together.</p>' +
    "<fieldset><legend>Agents data</legend>" +
    radio("demo-agents", "loaded", "Loaded (roster)", APP.demo.agents === "loaded") +
    radio("demo-agents", "loading", "Loading", APP.demo.agents === "loading") +
    radio("demo-agents", "empty", "Empty — no Agents exist", APP.demo.agents === "empty") +
    radio("demo-agents", "error", "Load failed — store unavailable", APP.demo.agents === "error") +
    "</fieldset>" +
    "<fieldset><legend>Service connection override (service/hosted Lanes)</legend>" +
    radio("demo-conn", "", "As fixtures define", APP.demo.conn === "") +
    radio("demo-conn", "stale", "Event stream reconnecting (stale)", APP.demo.conn === "stale") +
    radio("demo-conn", "disconnected", "Service disconnected", APP.demo.conn === "disconnected") +
    "</fieldset></div>" +
    '<div class="modal-actions"><button value="apply" class="btn btn-primary">Apply</button></div></form>';
  dlg.addEventListener(
    "close",
    function () {
      const agents = dlg.querySelector("input[name='demo-agents']:checked");
      const conn = dlg.querySelector("input[name='demo-conn']:checked");
      if (agents) APP.demo.agents = agents.value;
      if (conn) APP.demo.conn = conn.value;
      render();
    },
    { once: true }
  );
  dlg.showModal();
}

document.addEventListener("click", function (ev) {
  const t = ev.target.closest("[data-drawer], [data-drawer-close], [data-modal], [data-open-demo], [data-demo], [data-inspector-tab], [data-cu-tab]");
  if (!t) return;

  if (t.hasAttribute("data-cu-tab")) {
    APP.ui.cuTab = t.getAttribute("data-cu-tab");
    render();
    const btn = document.querySelector('[data-cu-tab="' + APP.ui.cuTab + '"]');
    if (btn) btn.focus();
    return;
  }

  if (t.hasAttribute("data-drawer")) {
    const id = t.getAttribute("data-drawer");
    APP.ui.drawer = APP.ui.drawer === id ? null : id;
    render();
    const btn = document.querySelector('[data-drawer="' + id + '"]');
    if (btn) btn.focus();
    return;
  }
  if (t.hasAttribute("data-drawer-close")) {
    const id = APP.ui.drawer;
    APP.ui.drawer = null;
    render();
    const btn = id && document.querySelector('[data-drawer="' + id + '"]');
    if (btn) btn.focus();
    return;
  }
  if (t.hasAttribute("data-inspector-tab")) {
    APP.ui.inspectorTab = t.getAttribute("data-inspector-tab");
    render();
    const btn = document.querySelector('[data-inspector-tab="' + APP.ui.inspectorTab + '"]');
    if (btn) btn.focus();
    return;
  }
  if (t.hasAttribute("data-open-demo")) {
    openDemoDialog();
    return;
  }
  if (t.hasAttribute("data-modal")) {
    const [kind, ...rest] = t.getAttribute("data-modal").split(":");
    openModal(kind, rest.join(":"));
    return;
  }
  if (t.hasAttribute("data-demo")) {
    const action = t.getAttribute("data-demo");
    if (action === "demo-agents-loaded") { APP.demo.agents = "loaded"; render(); }
    else if (action === "open-drawer-approvals") { APP.ui.drawer = "approvals"; render(); }
    else if (action === "assign-agent") { openModal("assign", parseHash().id + ":agent-1"); }
  }
});

document.addEventListener("change", function (ev) {
  if (ev.target && ev.target.id === "inspector-pin-select") {
    APP.ui.pinned = ev.target.value;
    render();
    const sel = document.getElementById("inspector-pin-select");
    if (sel) sel.focus();
  }
});

document.addEventListener("keydown", function (ev) {
  if (ev.key === "Escape" && APP.ui.drawer) {
    const id = APP.ui.drawer;
    APP.ui.drawer = null;
    render();
    const btn = document.querySelector('[data-drawer="' + id + '"]');
    if (btn) btn.focus();
  }
});

window.addEventListener("hashchange", function () {
  APP.ui.drawer = null; // drawers are per-screen
  render();
  focusHeading();
});

render();
