# Component and state inventory

Scope: the shared component vocabulary implemented in
[prototype/ui.js](prototype/ui.js) and used identically by all three
directions, plus accessibility notes and the responsive contract. Names
below match the prototype's CSS classes/functions so the eventual
implementation review can diff against something concrete.

## Components

| Component | Prototype source | Purpose | States |
|---|---|---|---|
| `stateChip` | ui.js | Icon + text label chip for any grammar state; color never the sole channel | all grammar states |
| `runtimeChip` | ui.js | Target icon + name + connection sub-label | connected, connecting, stale, disconnected |
| `stateCard` | ui.js | Full-area collection state; renders **exactly one** of loading / error / empty / content | loading, error (+ retry + Technical details), empty (+ constructive actions) |
| `banner` | ui.js | Lane-level condition strip: what happened, what is preserved, one primary action | warn, danger, info, review, muted |
| `contextHeader` | ui.js | The ownership rule: Lane · Agent · Runtime · Workspace · Run, each with a state chip | full + compact variants |
| `transcript` / `turn-*` | ui.js | User, agent, tool, test, event turns | test turns carry pass/fail tone |
| `composer` | ui.js | One prompt box; target line "Sends to *Lane* on *Runtime*"; self-disables with a stated reason. "Continue on another Runtime…" opens a D11 dialog: continuing creates or selects a different Lane, nothing moves | ready, blocked (archived / missing workspace / disconnected / retired-agent) |
| `drawerDock` + `drawer-panel` | views.js | Right-edge dock of seven Lane-scoped drawers; one open at a time; `aria-expanded`, Escape closes | open/closed; badges for approvals + queue counts |
| `drawerScopeLine` | ui.js | Mandatory first line of every drawer: "Scoped to *Lane* · *Agent/Ad hoc*" | — |
| `LaneScopeLine` | desktop/src/components/LaneScopeLine.tsx | Compact production ownership line for approvals, Run details, Tools, Terminal, and Computer Run surfaces: Lane · Agent · Runtime · Workspace · Run | connected, reconnecting, disconnected, unavailable |
| Queue & steering drawer | ui.js | Steering note to the active Run; ordered queued prompts with positions | running (steering visible), idle |
| Changes & tests drawer | ui.js | Changed files with +/− counts; observed test evidence with timestamp | changes/none; tests pass / not observed / none |
| Approvals drawer | ui.js | Approval card bound to Run id + file fingerprint; Review diff first / Approve / Deny | pending, none |
| Terminal drawer | ui.js | Terminal labeled with the Lane's workspace path | static representation |
| MCP & tools drawer | ui.js | Trust gate first, servers, doctor behind details | untrusted (fixture) |
| Computer Use drawer | ui.js | Two tabs. "Observed today": the audited unavailable state (permissions + store lock behind Technical details, named repair). "Contract illustration (#273)": contract-labelled operator control — Lane + Run, local app/window target, grant scope/expiry, model/provider, origin, action/time budgets, current action, observation freshness, sticky always-reachable Pause / Stop / Take over / non-cancelling Steer, approval bound to Lane + Run + action + target + fresh evidence, invalidation statement | unavailable (observed); operator-control (contract illustration, local only, never hosted) |
| Run history drawer | ui.js | Durable Runs with state, origin, execution mode, lineage (continues / retry-of), checkpoint inspect, progress | queued, running, completed, interrupted, awaiting approval |
| `agentCard` | views.js | Roster card: role, lifecycle **and** health chips, **current/last Lane runtime or aggregate** (never an Agent-wide Runtime — runtime belongs to Lanes), lane counts, current Lane, checkpoint or "none yet", actions with a visible "Proposed" marker on Pause/Retire | active/paused/retired; needs-attention (derived, C15) |
| Agent detail | views.js | Identity + Start-Lane form (runtime radio with live connection chips) + grouped Lanes + D04 Build-only ad-hoc assignment; same-source checkpoint continuation is validated, cross-workspace continuation and routine reassignment remain unavailable; lifecycle actions marked "Proposed" | active, paused, retired (banner + blocked form) |
| `laneRow` | views.js | Work record: title, objective, Agent/Ad hoc, workspace display name, runtime chip, status + next action, activity, indicator badges, Open/Archive/Restore | active, attention, archived (dashed + Restore) |
| Lane list toolbar | views.js | Active/Attention/Archived/All tabs with counts; identity-preserving search with live result count (`aria-live`) | — |
| Runtime target card | views.js | Name, connection, workspace authority, "What syncs", supports matrix, last seen, Technical details | connected, disconnected (+ Reconnect) |
| Modal (`<dialog>`) | app.js | Archive / Retire / Assign confirmations with consequence copy; Demo-states panel | — |
| Direction switcher | app.js | Persistent top-bar segmented control; preserves current screen across directions | — |

## Lifecycle-action grouping (F-05)

| Group | Actions | Placement | Confirmation |
|---|---|---|---|
| Continue | Open, Focus, Resume from checkpoint, Retry run | Primary buttons on rows/banners | none |
| Organize | Archive, Restore | Row/banner secondary | Archive dialog states what is preserved and that it is reversible |
| End of life *(proposed lifecycle — not implemented; visibly marked "Proposed")* | Retire (Agent), Proposed: unretire | Agent surfaces only | Retire dialog carries a "Proposed lifecycle contract (D05)" banner, contrasts itself with Archive, states that nothing is archived/deleted/moved/reassigned/rewritten, and **blocks confirmation** while the Agent has queued/running Runs or live isolated approvals (cancel, wait, or deny first) |
| Destructive | Delete permanently | **Not present** on rows/cards in any direction; reserved for a separate data-retention surface | out of scope this phase |

Pause/Unpause are likewise proposed lifecycle actions and carry the same
"Proposed" marker. Operational health (what is happening now) is always a
separate chip and is never affected by these actions.

## State machine coverage (handoff §6 → prototype)

| Required state | Where to see it |
|---|---|
| No Agents yet | `#/d2/agents?demo=empty` |
| No active Lanes | Lanes screen empty views (per-tab empty copy) |
| Refresh failed, store unavailable | `#/d2/agents?demo=error` — replaces roster/empty, Retry + Technical details with the audited `os error 35` text demoted |
| Missing local workspace | `#/d1/lane/lane-5` — banner + header chip + disabled composer, Choose folder |
| Disconnected remote service | `#/d2/lane/lane-10` (VM), or any service Lane with `?conn=disconnected` |
| Stale/reconnecting event stream | `#/d1/lane/lane-2?conn=stale` — "from last durable cursor" copy |
| Queued Run | `#/d1/lane/lane-9` — position shown, cancel offered |
| Awaiting approval, isolated diff | `#/d1/lane/lane-2?drawer=approvals` — bound to Run + fingerprint |
| Interrupted Run with verified checkpoint | `#/d1/lane/lane-4` — Resume vs Retry distinguished in copy |
| Archived Lane | `#/d2/lane/lane-6` and `lane-7` (scratch label hygiene), `lane-8` |
| Retired Agent | `#/d2/agent/agent-4` and its Lane `lane-8` (proposed lifecycle, marked) |
| Computer Use operator control (contract, #273/S14) | `#/d1/lane/lane-1?drawer=computer&cu=contract` — contract-labelled, local only; audited unavailable state remains the default tab |

## Accessibility notes

Implemented in the prototype (and expected of the production build):

- **Semantics:** landmarks (`header`, `nav` with labels, `main`, `aside`),
  one `h1` per screen, real `<button>`/`<a>` split by behavior, `<dialog>`
  for modals (native focus trap + Escape), `<details>` for Technical
  details, `<progress>` with a text label for run progress, a data `<table>`
  with `caption` + `scope` for the Lane↔runtime map.
- **Focus:** visible `:focus-visible` outline (2px accent) on every
  interactive element; route changes move focus to the screen's `h1`;
  opening/closing a drawer returns focus to its toggle; skip-link to main.
- **Names:** every icon is `aria-hidden` with an adjacent text label; where
  labels collapse at narrow width (drawer dock), the visually-hidden pattern
  keeps the name in the accessibility tree; `aria-current` marks active nav
  and tabs; drawer toggles carry `aria-expanded`/`aria-controls`; search
  results count is `aria-live="polite"`.
- **Color:** state tones satisfy ≥ 4.5:1 against their backgrounds in both
  themes (checked for body text; chip text is 11.5px semibold and also
  passes); state is always icon + label + text, never color alone.
- **Motion:** the only animations are the running-pulse and spinner;
  `prefers-reduced-motion: reduce` disables both and all transitions.
- **Theming:** dark-first using the product's own tokens; a light palette
  (the product's existing light tokens) applies via
  `prefers-color-scheme: light`; `color-scheme` is declared so form
  controls match.

Deferred to a production pass (not claimable from a static prototype):

- Full keyboard traversal order audit with a screen reader (VoiceOver/NVDA)
  on the real DOM, including virtualized long lists.
- Roving tabindex / arrow-key behavior inside the drawer dock and tab
  groups (prototype uses plain buttons/links in tab order, acceptable but
  not final).
- Live-region strategy for streaming transcript/Run updates and reconnect
  banners (prototype states are static; production needs rate-limited
  `aria-live` announcements for state transitions).
- Contrast verification of any future imagery/diff syntax highlighting.

## Responsive contract

| Breakpoint | Behavior |
|---|---|
| ≥ 1160px | Full layout: rail + main + drawer panel/Inspector inline |
| 880–1160px | Drawer panel becomes a right overlay (scrim-free, Escape closes); D3 zones stack vertically; Inspector moves below zones full-width; Agent detail columns stack |
| ≤ 880px | Rail becomes a horizontal scrollable strip under the app bar; lane rows single-column; context header wraps (chips stay adjacent to their values); composer stacks; dock is icon-only with names preserved for AT; direction switcher collapses to numbers. In the Computer Use contract state, the target block is sticky at the top and the Pause/Stop/Take over/Steer controls are sticky at the bottom of the drawer, so target and controls stay visible at every width |

The narrow layout was exercised at 760×1000 in headless Chrome (see
[captures](captures/CAPTURES.md)); no horizontal body scroll occurs at
760px, and every control remains reachable.
