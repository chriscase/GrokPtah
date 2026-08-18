# GrokPtah issue #308 — Phase 2 design package

Date: 2026-08-17
Author: Claude (Fable 5), acting as senior product designer for Phase 2
Status: design exploration for review — **no production code was changed**

This package answers the [Phase 2 design handoff](../../ux-audit/PHASE-2-DESIGN-HANDOFF.md)
with three coherent, meaningfully different design directions, one shared
product model, and a clickable dependency-free prototype.

Contract alignment: the package has been corrected against the independent
Grok Build product-contract review (`delegated/GROK-BUILD-CONTRACT-REVIEW.md`)
and the integration decision record (`integration/CONTRACT-INTEGRATION.md`).
Where those artifacts and this package disagree, the contract review's
repository facts win; decision IDs cited below (D05, D11, C15, S14, …) refer
to it.

## Package contents

| Artifact | What it is |
|---|---|
| [README.md](README.md) (this file) | Glossary, principles, annotated IA, shared state grammar, evidence boundary |
| [direction-1-focused-lane-workbench.md](direction-1-focused-lane-workbench.md) | Direction 1: calm single-Lane default with contextual drawers and an opt-in expert Grid |
| [direction-2-agent-operations-home.md](direction-2-agent-operations-home.md) | Direction 2: durable Agent roster and Agent detail as the primary organizing experience |
| [direction-3-adaptive-expert-workspace.md](direction-3-adaptive-expert-workspace.md) | Direction 3: professional multi-Lane supervision with scope made explicit everywhere |
| [comparison-and-recommendation.md](comparison-and-recommendation.md) | Independent comparison and a recommendation, separated from observed facts |
| [components-and-states.md](components-and-states.md) | Component/state inventory, accessibility notes, responsive contract |
| [prototype/](prototype/) | Static HTML/CSS/JS prototype with a visible direction switcher (no dependencies, no network) |
| [captures/CAPTURES.md](captures/CAPTURES.md) | Deterministic screenshot index (headless Chrome renders of the prototype) |

## How to preview the prototype

Open the file directly — there is no build step, dependency, or network access:

```bash
open docs/ux-design/phase-2/prototype/index.html
```

Or serve it (identical behavior):

```bash
python3 -m http.server -d docs/ux-design/phase-2/prototype 8123
```

- The **Direction** switcher in the top bar moves between the three
  directions, preserving the current screen where it exists.
- **Demo states** (top right) previews the shared state grammar:
  Agents loaded / loading / empty / load-failed, and a service-connection
  override (stale / disconnected) for Lanes on service runtimes.
- Every state is also deep-linkable, e.g.
  `#/d2/agents?demo=error`, `#/d1/lane/lane-2?conn=stale`,
  `#/d1/lane/lane-2?drawer=approvals`, and the Computer Use contract
  illustration `#/d1/lane/lane-1?drawer=computer&cu=contract`.

Representative fixtures: a running Lane (`lane-1`), an approval-gated
isolated diff (`lane-2`), an ad-hoc Lane (`lane-3`), an interrupted Run with
a verified checkpoint (`lane-4`), a missing workspace (`lane-5`), a queued
hosted Run (`lane-9`), a disconnected local service (`lane-10`), archived
Lanes including a scratch-workspace Lane (`lane-6`, `lane-7`, `lane-8`), and
a retired Agent (`agent-4`).

## Glossary (shared by all three directions)

These are the product words. All three directions use them identically; the
directions differ in **hierarchy**, not vocabulary.

| Term | Meaning | Source of truth |
|---|---|---|
| **Agent** | A durable identity: name, role, policy, memory, default model, checkpoint lineage. Lives across many Lanes. An Agent has no Runtime of its own — Runtime belongs to each Lane; Agent surfaces show the current/last Lane's runtime or an aggregate. | `AgentRecord` + [runtime model](../../AGENT_LANE_RUNTIME_MODEL.md) |
| **Lifecycle** (Agent) | A user decision: Active, Paused, Retired. **Proposed contract, not implemented** — the runtime's enum today is operational health only (`active\|waiting\|interrupted\|failed\|completed`). Pause/Retire/Unretire are marked "Proposed" throughout the prototype. | contract review §2.4 |
| **Health** (Agent) | What is happening now: Ready, Running, Waiting, Interrupted, Failed. "Needs attention" is derived from Lane state for presentation, never stored on the Agent (C15). Never conflated with lifecycle. | runtime model + contract review |
| **Lane** | A high-turnover work context: objective, workspace, branch/worktree, transcript, queue, Runs, approvals, changes, tests, runtime target. Frequently archived. May be **ad hoc** (no Agent). | Lane projection over `SessionSummary` |
| **Run** | One durable execution inside a Lane, with progress, interruption, checkpoint, retry/continuation lineage, diff/test evidence, and approval state. | `RunRecord` |
| **Checkpoint** | A verified continuation boundary produced by a Run; belongs to Agent continuity, attributable to the Lane/Run that made it. Resume from it is always explicit. | `ContinuationCheckpoint`, [persistent agent protocol](../../PERSISTENT_AGENT_PROTOCOL.md) |
| **Runtime target** | Where a Lane executes: **Local desktop**, **Local service / VM**, or **Hosted service**, each with its own connection state and sync policy. | [headless service](../../HEADLESS_SERVICE.md) |
| **Archive (Lane)** | Reversible removal from Active views. Preserves transcript, Runs, checkpoints, approvals, evidence, and Agent relationship. Blocks new work until Restore. | runtime model, archive semantics |
| **Retire (Agent)** | Ends an identity's working life. Blocks new Lanes/Runs under that Agent. Preserves memory, checkpoints, and historical Lanes. **Not** the same action, surface, or copy as Archive. | runtime model, lifecycle |

## Design principles

Derived directly from the Phase 1 findings (F-01 … F-12) and the Fable 5
review; each principle names the findings it answers.

1. **Identity first, work second, execution third.** Agents (durable) and
   Lanes (disposable) are first-class navigation objects; sessions/zones/tabs
   are implementation vocabulary that never appears in primary labels.
   *(F-02, F-04)*
2. **Every contextual surface names its Lane.** Composer, drawers, terminal,
   diffs, approvals, queue, Computer Use, and MCP always render an explicit
   Lane scope. Nothing infers ownership from tab focus. *(F-03)*
3. **One state per view, one next action per state.** Error, empty, loading,
   and content are mutually exclusive renders. Every non-ready state names
   its single primary repair action. Raw paths, transports, and OS error
   numbers live behind "Technical details." *(F-01, F-10)*
4. **Lifecycle actions are separated by consequence.** Continue (Open,
   Resume, Fork) ≠ organize (Archive/Restore) ≠ end-of-life (Retire, Delete).
   Different groups, different visual weight, different confirmations, and
   confirmation copy that states what is preserved. *(F-05)*
5. **Runtime is visible before it matters — and belongs to the Lane.** The
   target and its connection state appear in the Lane header, in the composer
   ("Sends to X on Y"), and at Lane creation — never only in a settings panel
   and never as an Agent-wide property. Continuing an objective on another
   Runtime is an explicit action that creates or selects a different Lane;
   nothing is silently retargeted and no files, terminals, credentials, or
   live Runs move (D11). Sync claims are limited to what the service contract
   documents; local-desktop copy distinguishes local persistence from prompts
   sent to the configured model provider. *(F-06, F-07)*
6. **Progressive disclosure, not capability removal.** Terminal, MCP,
   Computer Use, queue/steering, diffs/tests/approvals, worktrees, and run
   history all remain reachable within two interactions from a focused Lane —
   as drawers/inspectors, not as permanent peer panels. *(F-04, F-09)*
7. **Human names, technical truth on demand.** Scratch `.tmp*` paths,
   store paths, and bridge internals are demoted to Technical details;
   user-facing labels are work-record names ("Scratch workspace"). *(F-08 via
   Lane-identity search results, Fable finding 6)*

## Annotated information architecture

Shared spine (all directions expose these objects and screens; the
**bold** element is what differs — the default orientation surface):

```text
GrokPtah
├── Agents ................ durable roster; lifecycle + health, current-Lane runtime,
│   │                       lane counts, checkpoint; Create / Pause / Retire
│   │                       (proposed lifecycle; always separate from Archive)
│   └── Agent detail ...... identity + memory/policy; Lanes grouped Active /
│                           Attention / Archived; Start Lane (runtime chosen here);
│                           adopt ad-hoc Lanes
├── Lanes ................. work records, not anonymous sessions
│   ├── Active / Attention / Archived / All views
│   └── Search ............ results keep Lane identity (title, Agent, workspace, state)
├── Focused Lane .......... context header (Lane · Agent · Runtime · Workspace · Run)
│   ├── Transcript + one composer with an explicit target
│   ├── Drawers: Queue & steering · Changes & tests · Approvals · Terminal ·
│   │            MCP & tools · Computer Use · Run history
│   └── One state banner with one next action when not ready
├── Multi-Lane surface .... 2+ zones, each with its own header and composer;
│   │                       inspector explicitly pinned, never focus-following
│   └── (D1: opt-in "Expert Grid" · D3: the default "Supervision workspace")
├── Runtime targets ....... Local desktop / Local service-VM / Hosted service;
│                           connection, workspace authority, what syncs, support matrix
└── Settings .............. unchanged scope for this phase (defaults, permissions,
                            appearance, auth) — not re-designed in this package
```

- **Direction 1** starts the user in *Focused Lane* (work-first).
- **Direction 2** starts the user in *Agents* (identity-first).
- **Direction 3** starts the user in the *multi-Lane supervision surface*
  (oversight-first) with density controls.

## Shared state grammar

Implemented in the prototype as one chip/banner/state-card vocabulary
(`data.js: STATE_GRAMMAR`, `ui.js: stateChip/stateCard/banner`). Color is
never the only channel — every state has an icon and a text label.

| State | Tone | Where it appears | Primary action |
|---|---|---|---|
| Loading | muted spinner | any collection | none (bounded) |
| Ready / Waiting | neutral | Lane, Agent health | send prompt |
| Running | gold pulse | Lane, Run, Agent health | view progress / steer / cancel |
| Queued | blue | Run, Lane banner | cancel or wait (position shown) |
| Awaiting approval | purple | Lane banner, Approvals drawer, roster badges | review diff → approve / deny |
| Blocked / Workspace missing | orange | Lane banner + header chip | the named repair (e.g. Choose folder) |
| Interrupted | orange | Lane banner, Run history | resume from verified checkpoint / retry / inspect |
| Reconnecting (stale) | orange | Lane banner | refresh; data labeled "from last durable cursor". Note: `reconnecting` is the wire value; "stale" is presentation of last-known data, never a stored connection state (C2) |
| Disconnected | red | runtime chip, Lane banner | reconnect / switch runtime |
| Failed | red | Run, refresh errors | retry + Technical details |
| Unverified | orange | tests/changes | run or observe tests |
| Completed | green | Run | inspect evidence |
| Archived | gray box | Lane rows, Lane banner | restore / inspect (never implies deletion) |
| Retired | gray moon | Agent | inspect / unretire; new work blocked with copy |
| Empty | dashed card | any collection | the constructive next step |
| Load failed | red card | any collection | retry; **replaces** empty state, never beside it |

## Evidence boundary

Observed (Phase 1 audit, kept as authoritative):

- Session-first navigation with 102–103 sessions, 8 Live sessions, dense
  default cockpit, `.tmp*` labels, duplicated controls.
- Contradictory error+empty presentations for Persistent Agents / Task Runs,
  raw `os error 35` lock messages with internal paths.
- Computer Use permissions **not granted** and its store locked; no
  successful capture observed.
- No hosted/cloud session exercised end-to-end; no reconnect-after-loss or
  second-device handoff observed.

Design intent in this package that is **not** audit-observed and must be
validated in a product walkthrough:

- All hosted-service and local-service/VM screens (connected, queued,
  reconnecting, disconnected) illustrate the documented
  [headless service contract](../../HEADLESS_SERVICE.md) — durable ledger,
  allowlisted workspaces, cursor recovery, explicit resume — not observed
  product behavior. Every service/hosted Lane surface carries this note
  inline, in the viewport, not only in a footer. Nothing claims that
  transcripts, source files, terminals, credentials, clipboard, or Computer
  Use authority synchronize.
- Agent Pause / Retire / Unretire are **proposed lifecycle behavior**
  (contract review §2.4), visibly marked "Proposed" in the prototype. The
  retire dialog demonstrates the proposed D05 eligibility gate: retirement
  is blocked while queued/running Runs or live isolated approvals exist.
- Computer Use appears in its audited unavailable state (default), plus one
  **contract-labelled operator-control illustration** for issue #273 /
  rubric S14 (`?drawer=computer&cu=contract`): exact Lane + Run, local
  app/window target, grant scope and expiry, model/provider, origin,
  action/time budgets, current action, observation freshness,
  always-reachable Pause / Stop / Take over / non-cancelling Steer, and a
  review state bound to Lane, Run, action, target, and fresh evidence.
  Restart, target change, or stale observation invalidates authority. This
  is design-contract illustration, never an observed capture, and never
  hosted.
- All fixture content (agents, lanes, runs, transcripts, timestamps) is
  illustrative.

## Constraints honored

- No production application code, build configuration, or runtime contracts
  were modified. No existing audit evidence was edited.
- The package lives entirely under `docs/ux-design/phase-2/`.
- The prototype is dependency-free (plain HTML/CSS/JS, no fetch, no CDN) and
  works from `file://`.
