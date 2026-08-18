# GrokPtah Phase 2 product-contract review

**Reviewer:** Grok Build (Grok 4.6)  
**Date:** 2026-08-17  
**Issue:** [#308](https://github.com/chriscase/GrokPtah/issues/308)  
**Related PR:** [#309](https://github.com/chriscase/GrokPtah/pull/309) (draft; head `codex/lane-context-foundation-20260817`)  
**Working branch:** `codex/ux-phase2-prototypes-20260817`  
**Kind:** Independent design-contract analysis. No production code, runtime, or merge state was changed.

This package is a product contract, not a visual prototype and not an implementation patch. It separates repository facts from recommendations. Where the existing design package and the running code disagree, the code and its tests are treated as current behavior; the design package is treated as intent.

Evidence hierarchy used here matches `docs/ux-design/phase-2/integration/SCENARIO-RUBRIC.md`:

1. Current repository contracts and executable behavior.
2. Phase 1 walkthrough observations.
3. Issue #308 product requirements.
4. Claude / Fable 5 review.
5. Prototype recommendations.

Hosted end-to-end behavior, second-device handoff, a successful Computer Use capture, and a live mutating approval modal were **not** observed in Phase 1. This review does not invent those successes.

---

## 1. Repository facts

### 1.1 What exists today

| Object | Durable record | Identity today | What it actually stores |
|---|---|---|---|
| **Session** | `SessionSummary` / `SessionMeta` under `~/.grokptah/sessions/<uuid>/` | `session.id` | Title, cwd, kind (`build` \| `chat`), tags/folder, archive flags, optional `agent_id`, execution mode, workspace status, transcript, prompt queue, completion history |
| **Lane** | Projection only | `lane_id = session_id` during compatibility | `LaneSummary` is derived from `SessionSummary`. It adds `runtime_target` and `runtime_connection`. It is not a separate store entity |
| **Agent** | `AgentRecord` in the orchestration store | `agent_id` | Primary `session_id`, `lane_ids`, one `workspace`, one `model`, operational `state`, `current_run_id`, `latest_checkpoint_id`, ordinal, timestamps |
| **Run** | `RunRecord` | `run_id` | `session_id`, optional `agent_id`, `retry_of`, `parent_run_id`, `queue_position`, bounds, preview, journal range, aggregates, progress, isolated execution, isolated approval |
| **Checkpoint** | `ContinuationCheckpoint` | `checkpoint_id` | Agent, **one** `session_id`, producing `run_id`, workspace, redacted context, hash, ordinal, reason |
| **Runtime target** | Projection | `local_desktop` \| `local_service` \| `hosted_service` | Wired on `LaneSummary` and remote-service status. Local sessions default to `local_desktop` + `connected` |
| **Workspace** | Path + `WorkspaceStatus` | Lane cwd, or service allowlist entry | `ready` \| `missing` \| `inaccessible` \| `not_directory` |
| **Follow-up queue** | `prompt_queue.json` per session | `entry_id` + `version` + queue `revision` | Durable follow-ups for one Build session; CAS mutators; survives host restart of the **same** home |
| **Admission queue** | In-process host ledger + `RunState::Queued` | `run_id` + `queue_position` | Process-wide cap of 32 pending admissions. Prompts are **not** durable across process restart |
| **Tool permission** | Live `permission_required` event | `PermissionRequest.id` + `session_id` | In-turn tool gate. Not the isolated-promotion approval |
| **Isolated approval** | `RunApproval` on the Run | `approval_id` | Bound to run, session, workspace, source/final fingerprints, exact files. Default TTL 5 minutes, max 15 |
| **Computer Use grant** | Separate computer-use store | Computer Run + grant epoch | Local only. Not an MCP mutation. Grants die on restart, pause, cancel, target change |
| **Archive** | `SessionMeta.archived` + `archived_at` | Same session/lane id | Hides from default lists, closes tabs, keeps transcript and Agent binding |
| **Delete** | `session_store::delete_session` | Session directory removed | Deletes meta, transcript, and prompt queue. Does **not** delete `AgentRecord`, `RunRecord`, or checkpoints |

### 1.2 Implemented identity and lifecycle enums

**Agent operational state** (`AgentState` / `PersistentAgentState`):

`active` | `waiting` | `interrupted` | `failed` | `completed`

`can_resume` is true only for `waiting`, `interrupted`, and `failed`.

There is **no** implemented Agent lifecycle of `paused` or `retired`. Those words exist in the design package and, separately, on Computer Use runs.

**Run state** (`RunState`):

`queued` → `running` → `completed` | `failed` | `cancelled` | `interrupted` | `limit_reached`

`interrupted` is written when the orchestration store is reopened while a run is `queued` or `running`. Interrupted runs are inspectable and are never resumed automatically.

**Workspace status:**

`ready` | `missing` | `inaccessible` | `not_directory`

Control-plane mutations refuse a non-ready workspace (`WorkspaceMismatch`).

**Runtime connection** (implemented):

`connected` | `reconnecting` | `disconnected` | `error`

The design model also names `stale`, `unknown`, and `connecting`. Those are **not** wire values today.

**Lane status** in `AGENT_LANE_RUNTIME_MODEL.md` (`draft` | `ready` | `running` | …) is a proposed product projection. It is **not** stored on `SessionSummary` or `LaneSummary`.

### 1.3 One Agent, many Lanes — what actually landed in PR #309

PR #309 and its foundation commits added a real compatibility projection:

- `LaneSummary.id == session_id`.
- `AgentRecord.lane_ids` plus `known_lane_ids()`, which synthesizes the legacy primary `session_id` when `lane_ids` is empty.
- `Host::attach_session_to_agent` appends a Build session to `lane_ids` and writes `session.agent_id`.
- `RunRecord::lane_id()` returns `session_id`.
- Archive keeps `session.agent_id`. `list_lanes(false)` hides archived Lanes; `list_lanes(true)` retains them.
- `WorkspaceUiState.active_lane_id` is a projection of `active_session`. Persisted chrome is still `workspace.json` → `active_session`.
- Desktop Tools / composer / run inspector began taking an explicit Lane scope. This is started, not complete.
- `StateCard` now presents empty and error as mutually exclusive on Persistent Agents and Task Runs.

Hard limits that still contradict the product story:

1. **Resume is still bound to the Agent’s primary `session_id` and primary `workspace`.**  
   `AgentResumePlan::validate_for` requires `agent.session_id`, `checkpoint.session_id`, and both workspaces to match the requested session. `Host::ensure_session_agent` refuses if `agent.session_id != session_id` or `agent.workspace != workspace`.
2. **An Agent still has one `workspace` field.** Secondary Lanes may have different cwds. Attaching them does not retarget the Agent record.
3. **A checkpoint is produced for the Run’s `session_id`.** If the latest checkpoint was written from a secondary Lane, resume against the primary session fails closed.
4. **Default Agent ids are `agent-{session_id}`.** Identity is still born from the first session.
5. **Fork does not copy `agent_id`.** `fork_session` copies transcript, cwd, model, effort, plan, and kind, sets `forked_from`, and creates an ad-hoc Lane.
6. **Chat sessions cannot own or attach a persistent Agent.**
7. **Opening an archived session from Browse auto-unarchives it.** Restore is not an explicit operator step today.
8. **Archive does not block control-plane submit, retry, resume, or queue.** `require_build_session` checks kind and workspace readiness, not `archived`.
9. **“Run on” a remote service submits to a different `session_id`.** It does not retarget the focused local Lane. `projectRemoteSessionAsLane` invents `message_count: 0`, `archived: false`, `workspace_status: "ready"`.
10. **Local wins identity collisions.** `mergeLaneProjections` keeps the local Lane when ids match.

### 1.4 Two queues, three approvals

These must not be collapsed in UI language.

| Mechanism | Scope | Survives process restart? | User job |
|---|---|---|---|
| Follow-up prompt queue | One Build session / Lane | Yes, in `prompt_queue.json` of the same home | “Do this next in this Lane” |
| Admission queue | Host-global, max 32 | No. Durable record becomes `interrupted`; in-memory prompt is gone | “Wait for a free run slot” |
| Tool permission | Live turn, `session_id` | No | “May this tool run now?” |
| Isolated promotion approval | One completed isolated Run + fingerprints | Yes, until TTL expires | “Apply these exact files to the source workspace?” |
| Computer Use grant | One local Computer Run | No | “May this local observation/action proceed?” |

Steering is not a queue item and not a new Run. It injects at the next safe model boundary (`pending` while a turn is active, `queued` if idle). Clearing the follow-up queue can cancel accepted-but-not-injected steering; already-injected steering cannot be retracted.

### 1.5 Runtime and service facts

- Desktop default execution is the in-process bridge (`local_desktop`).
- `grokptah-service` starts the same bridge + authenticated MCP control plane. It owns process lifecycle and config only.
- Service workspaces are allowlisted. `ptah_create_session` cannot mint a session on an arbitrary path.
- Remote HTTP is loopback-only unless `--allow-remote` plus a ≥24-character bearer token. Network clients must use HTTPS.
- The desktop holds the service token in Tauri backend memory. The web UI does not persist it.
- One service process, many MCP transport sessions, one durable ledger. Cross-session run reads/cancels fail closed. Unknown session reads fail closed.
- Live SSE is run-scoped. A gap emits `ptah_recovery` and closes the stream. Coordinators must poll `ptah_get_events` before reconnecting. Expired cursors return `cursor_expired` (HTTP 410) and must not skip retained history.
- Queued runs have no event range yet. Opening their live stream is a structured conflict.
- Client disconnect does **not** stop a service-owned Run.
- `OrchStore` and the Computer Use store take exclusive filesystem locks. Two processes on the same `GROKPTAH_HOME` produce the Phase 1 `already open (os error 35)` failure.
- Computer Use is not an MCP mutation surface. Authority is local. Observation and action are separate.

### 1.6 What Phase 1 actually observed

Observed on a local desktop build:

- Session-first navigation (102–103 sessions, 8 Live sessions).
- Missing-workspace recovery with a clear next action.
- Persistent Agents and Task Runs showing a store-lock error **and** an empty-state claim (F-01). PR #309’s `StateCard` is the start of a fix; Phase 1 screenshots remain the visual baseline.
- Tools/Git appearing to belong to a different zone than the focused composer (F-03).
- Computer / Computer Run / Computer Use / Simulator naming collision (F-06).
- No hosted session, no reconnect-after-loss, no second-device handoff, no successful Computer Use capture, no live mutating approval modal, no narrow-window pass.

### 1.7 Related but not-yet-product objects

Issue #308 names workloads and routines. Those belong to [#305](https://github.com/chriscase/GrokPtah/issues/305) and [#297](https://github.com/chriscase/GrokPtah/issues/297) / epic #301. They are **not** present as durable product objects in the current runtime. Phase 2 must reserve space, not invent a workload or routine state machine.

---

## 2. Design recommendations

These are contract recommendations for prototypes and later implementation slices. They are not claims that the current UI already behaves this way.

### 2.1 Canonical product objects

Keep exactly these user-facing objects:

| Object | Lifetime | User question | Lifecycle verbs |
|---|---|---|---|
| **Agent** | Months to years | Who is this identity, and what persists with it? | Create, Pause, Unpause, Retire, Inspect |
| **Lane** | Hours to weeks | What objective and workspace are we in? | Start, Archive, Restore, Fork, Delete permanently |
| **Run** | Minutes to hours | What execution is active, blocked, or finished? | Submit, Cancel, Retry, Inspect |
| **Runtime** | Selected for a Lane | Where will the next action execute? | Connect, Reconnect, Choose another Runtime Lane |
| **Workspace** | Bound to a Lane | What files can this Lane touch? | Choose folder, Repair |
| **Checkpoint** | Produced by a finished Agent Run | Where is the verified continue point? | Inspect, Resume from checkpoint |

**Session** remains an implementation and compatibility word. It may appear under Technical details, in MCP payloads, and in migration notes. It is not the default navigation noun.

**Ad hoc** means a Lane with no Agent. It is a complete, valid state. Never say “Ad hoc Agent.”

### 2.2 Non-negotiable relationship rules

1. Agents are durable identities. Lanes are high-turnover work contexts. Runs are durable executions inside a Lane.
2. One Agent may own many Lanes. Ownership is `session.agent_id` plus `AgentRecord.lane_ids`.
3. A Lane may be ad hoc. Creating a Lane must not create an Agent.
4. Assigning an Agent is explicit. It does not rewrite historical transcript or Run records.
5. Archiving a Lane is reversible and preserves transcript, Runs, events, checkpoints, diffs, tests, approvals, and Agent binding.
6. Retiring an Agent is a different verb from archiving a Lane. Retire does not archive, delete, or rewrite Lanes.
7. Every composer, terminal, tool panel, approval, diff, test result, queue, steering control, Computer Run surface, and MCP surface has an explicit Lane id. Global tab focus is not ownership.
8. Local desktop execution and service-owned execution are visually and verbally distinct.
9. Empty and error are exclusive presentations of the same collection.

### 2.3 Assignment, fork, rewind, delete

**Assign Agent to an existing Lane (D04).**

- Allowed for Build Lanes only.
- Writes current owner (`session.agent_id`) and appends the Lane to `agent.lane_ids`.
- Does not rewrite transcript, Runs, or checkpoints.
- Does not change the Agent’s primary resume session or primary workspace in this migration slice.
- Reassignment from Agent A to Agent B is **not** a routine action until #297 adds attributable history. The prototype may show “Assign Agent” on an ad-hoc Lane; it must not offer casual Agent-to-Agent transfer.
- Previous Agent `lane_ids` membership is not silently rewritten today. Do not hide that gap. Label current owner as current owner.

**Fork.**

- Creates a new Lane.
- Copies transcript and workspace metadata.
- Starts ad hoc unless the user explicitly assigns the same Agent after the fork.
- `forked_from` is historical context, not shared live state.

**Rewind.**

- Mutates the same Lane.
- Conversation rewind truncates the local transcript.
- File rewind restores edit snapshots.
- This is not Archive, not Resume, and not Retry.

**Delete permanently.**

- Destroys the session directory (transcript, meta, follow-up queue).
- Does not delete Agent, Run ledger, or checkpoints. Those records can be orphaned.
- Until a retention policy exists, the confirmation copy must say that durable Run/Agent history may remain in the orchestration store.
- Refuse delete while a turn is active (already implemented).

### 2.4 Pause and retire (D05)

These are product lifecycle states. They are **not** implemented. Prototypes may show them; production must not pretend they already exist.

| Action | Allowed now? | Recommended contract |
|---|---|---|
| **Pause** | No | Agent remains visible. New Lanes and new Runs for this Agent require Unpause. Existing Lanes stay readable. In-flight Runs are not cancelled by Pause. Unpause is explicit |
| **Retire** | No | Agent remains inspectable. Cannot start Lanes or Runs. Does not archive Lanes. Block Retire while this Agent has `queued`/`running` Runs or an unexpired isolated approval. Operator must cancel, wait, or deny first |
| **Unretire** | No | Allowed in this phase. Treat as returning to `paused` or `active` after a confirmation. A later retention policy may forbid it |

Computer Use `paused` is a **Computer Run** state. Never reuse that word as the Agent lifecycle without a qualifier (“Computer Run paused”).

### 2.5 Archive and restore

Recommended product rules, including corrections to current behavior:

1. Archive removes the Lane from Active and Attention. It remains in Archived and All, and in search.
2. Archive preserves history. Copy: “History is kept. You can restore this Lane later.”
3. Archive does not touch the Agent, workspace files, or managed worktrees.
4. **Inspecting an archived Lane must not restore it.** Current Browse-open auto-unarchive is a contract defect.
5. Restore is the only path back to Active.
6. After restore, Resume / Retry / Start new remain separate explicit actions.
7. While archived, the Lane cannot submit, resume, retry, queue, steer, or start Computer Use. Control plane should enforce this; UI should not be the only gate.
8. Bulk archive is Lane-only. Never pair it with bulk retire.

### 2.6 Resume vocabulary (D15)

Never use one button labeled Resume for all of these:

| User action | Runtime meaning | Preconditions | New object |
|---|---|---|---|
| **Reconnect** | Client reattaches to a service-owned ledger and event cursor | Service reachable; bearer valid | No new Run |
| **Resume from checkpoint** | `ptah_resume_persistent_agent` / `resume_agent` | Agent `waiting` \| `interrupted` \| `failed`; verified latest checkpoint; requested Lane is the Agent’s **current resume Lane**; workspace matches | New Run with `parent_run_id` |
| **Retry interrupted Run** | `ptah_retry_run` | Source Run is `interrupted`; same Lane, workspace, execution mode; fresh prompt | New Run with `retry_of` |
| **Start new Run** | Desktop prompt or `ptah_submit_task` | Lane ready; workspace ready; Runtime connected; Lane not archived; Agent not paused/retired if assigned | New Run, no parent/retry unless continuation is explicit |
| **Start on another Runtime** | Create or open a service-owned Lane and submit there | Allowlisted workspace on that service | **Different Lane** (`session_id`) |

Resume always requires a fresh prompt. The original prompt is not stored for automatic replay.

Until resume is unbound from `AgentRecord.session_id`, the UI must not promise “Resume this Agent in any of its Lanes.” The honest copy is: “Resume continues in the Agent’s primary Lane.” That is a migration blocker, not a wording trick.

### 2.7 Runtime target (D08, D11)

A Lane has one Runtime at a time. That Runtime is where the **next** action executes.

- Creating a Lane includes choosing Local desktop, Local service, or Hosted service.
- An in-flight Run stays on the Runtime that started it.
- The current composer “Run on” control targets a **different service session**. Product copy must say so: “Submit on [service Lane name],” not “Change this Lane’s Runtime.”
- Changing Runtime of the same logical work is **not** a silent selector. It is “Continue this objective on another Runtime,” which creates or selects another Lane and optionally assigns the same Agent. Do not imply files, terminals, or credentials move.
- Local desktop Lanes whose store is the desktop home stay `local_desktop` even if a service is also connected.

### 2.8 Ownership header

Every contextual surface renders the same scope, in this order:

```text
Lane: <title>                         <derived status>
Agent: <name or Ad hoc>               <lifecycle if implemented; else omit>
Runtime: <Local desktop | Local service | Hosted service>   <connection>
Workspace: <display name>             <Ready | Unavailable>
Run: <active run or No active run>    <queued | working | …>
```

If two Lanes are open, each zone and each drawer carries its own header. A shared Tools column that silently follows “the last focused tab” is a contract failure.

The composer target must be the Lane that will execute. If the operator submits to a service-owned Lane, that service Lane is the composer’s Lane. Do not keep the local Lane title as the primary identity while sending the prompt elsewhere.

### 2.9 State grammar

Primary language names **impact + next action**. Technical details hold paths, OS errors, bridge versions, transport, tokens, cursors, and request ids.

Required visual/semantic variants for any collection (Agents, Lanes, Runs, events):

| Variant | When | May combine with empty? |
|---|---|---|
| Loading | Request in flight, no reliable payload yet | No. Loading replaces empty |
| Empty | Reliable payload, zero items | No |
| Error | Request failed | No |
| Stale | Showing last-known payload while reconnecting or after a failed refresh | Stale may overlay a previous non-empty list. It must not claim “none exist” |
| Archived | The Lane itself is archived | N/A |

Never show “No persistent agents yet” beside “Could not refresh.” `StateCard` is the shared component; remaining surfaces must adopt it.

### 2.10 Search and naming

Search results are Lane records: title/objective, Agent or Ad hoc, workspace display name, Runtime, status, last activity, archived flag, and a short excerpt. Raw `session_id` and `.tmp*` paths are secondary.

Scratch directories are never the primary Lane name. Prefer the user title, then the project display name.

### 2.11 Computer Use

Computer Use remains owned by #273. Phase 2 only requires:

- The cockpit is a Lane-scoped contextual surface, not a global mode.
- Terminology: **Computer Use** is the capability; **Computer Run** is one bounded local run; **Simulator** is the offline test double. The top-level toggle must not say “Off” while a cockpit is open without explaining that Off is the lane capability, not the window.
- No implication that Computer Use authority, screenshots, or grants synchronize to a service or second device.
- Do not prototype a successful hosted Computer Use flow.

### 2.12 Workloads and routines

Do not add first-class Workload or Routine navigation in Phase 2 prototypes except as a disabled or “coming from #305” placeholder. Mapping them to Lanes or Runs will recreate the session-first confusion.

### 2.13 Information architecture (for prototypes, not this contract’s visual job)

This review does not pick a visual direction. It constrains all three:

- Agents and Lanes are both permanently reachable (D02).
- Default density is one focused Lane and one composer.
- Expert Grid / Live / Tools remain available and must carry per-zone Lane scope (D18).
- Landing destination (D01) is a prototype question, but every landing must handle: no Agents + ad-hoc Lanes; Agents + all Lanes archived; load failure.

---

## 3. State-transition matrix

### 3.1 Agent

#### Operational health (implemented)

```text
(created) → waiting
waiting → active          when a Run starts for this Agent
active  → waiting         terminal success / cancel / limit; checkpoint written
active  → failed          terminal failed Run; checkpoint still written
active  → interrupted     store reopen while a bound Run was queued/running
waiting | interrupted | failed → active   explicit Resume from checkpoint
completed                 residual / unused as a start state for new work
```

Opening the orchestration store:

- queued/running Runs → `interrupted`
- bound Agents → `interrupted` if their Run was unfinished
- latest verified checkpoint is retained
- `current_run_id` is cleared

#### Lifecycle (recommended; not implemented)

```text
(none) → active
active → paused → active
active → retired
paused → retired
retired → paused or active     only via explicit Unretire in this phase
```

Operational health and lifecycle are independent. An `active` Agent may be `interrupted`. A `retired` Agent may still show last health `waiting`. Retired/paused Agents cannot start new Lanes or Runs.

### 3.2 Lane

Lane status is **derived**. Do not persist a second source of truth in this migration.

Derivation order (first match wins):

1. `archived` → **Archived**
2. Load/refresh failed and no last-known Lane → **Unavailable** (error, not empty)
3. Runtime `disconnected` or `error` → **Disconnected**
4. Runtime `reconnecting` → **Reconnecting** (last-known status may be shown as stale)
5. Workspace not `ready` → **Workspace unavailable**
6. Current Run `awaiting isolated approval` or live tool permission → **Needs review**
7. Current Run `queued` → **Queued**
8. Current Run `running` → **Working**
9. Latest Run `interrupted` → **Interrupted**
10. Latest Run `failed` → **Failed**
11. Assigned Agent is paused → **Agent paused**
12. Assigned Agent is retired → **Agent retired**
13. No messages and no Runs → **Draft**
14. Else → **Ready**

```text
draft → ready                 workspace chosen, Runtime connected
ready → queued | working
queued → working | interrupted | cancelled
working → needs_review | ready | failed | interrupted | cancelled | limit_reached
needs_review → ready | failed
any active → archived         Archive
archived → ready              Restore, then re-derive
ready | working → workspace unavailable
ready | working → disconnected
```

Archive from any non-deleted state is allowed. Recommended: if a Run is `running` or `queued`, Archive requires confirmation that new work will be blocked; it does **not** cancel the Run on a service-owned Runtime. Local-desktop archive should warn that closing the Lane view does not stop an in-flight local Run unless the operator also cancels.

### 3.3 Run

```text
submit (capacity free)     → running
submit (allow_queue)       → queued → running
submit (no queue slot)     → rejected (capacity_exhausted); no Run
running → completed | failed | cancelled | limit_reached
queued | running + store reopen → interrupted
interrupted + retry        → new Run (retry_of = old)
checkpoint + resume        → new Run (parent_run_id = checkpoint.run_id)
cancelled queued           → cancelled; wasQueued = true; no model turn
```

Illegal:

- Retry of a non-interrupted Run.
- Retry that changes isolated vs shared mode.
- Resume of an `active` Agent.
- Resume with a tampered or missing checkpoint.
- Implicit resume after restart.
- Opening a live event stream on a queued Run that has no `startSeq`.

### 3.4 Runtime target and connection

```text
(none) → connecting/reconnecting → connected
connected → reconnecting → connected
connected | reconnecting → disconnected
any → error
```

Target kind does not change because the client disconnected. A hosted Lane that loses its stream stays `hosted_service`.

`stale` is a **presentation** of last-known Lane/Run data during `reconnecting` or after a failed refresh. It is not a fourth stored connection value.

### 3.5 Workspace

```text
ready
ready → missing            path no longer exists
ready → not_directory      path exists but is not a directory
ready → inaccessible       permission / IO failure
missing | inaccessible | not_directory → ready     Choose folder / repair
```

Non-ready blocks tools, model turns, queue, steer, submit, resume, and retry.

### 3.6 Follow-up queue

```text
enqueue → pending entry
steer_queued → pending (turn active) or queued (idle)
run_next → may cancel observed active turn, then start that entry
edit / reorder / remove → CAS on entry version; reorder also fences on queue revision
clear → removes entries; may cancel not-yet-injected steering
stale version or revision → no change; operator re-reads
```

### 3.7 Isolated approval

```text
isolated Run completed → reviewable
review + approve → RunApproval (TTL 5–15 min)
approve + promote (fingerprints still match) → promoted
source/worktree changed → conflicted
discard → discarded worktree; source untouched
TTL expiry → approval unusable; review again
```

Promotion is never implied by “the Agent finished.”

### 3.8 Computer Run (local only; #273)

Relevant here only so Phase 2 does not collide names:

`awaiting_authorization` → granted → observing/acting → `paused` | `stopped` | `interrupted` | `uncertain_outcome`

`operator_takeover` is absorbing. Reconnect cannot restore agent authority.

---

## 4. Local / hosted ownership matrix

### 4.1 Who owns what

| Concern | Local desktop | Local service / VM | Hosted service | Several clients on one hosted service |
|---|---|---|---|---|
| Agent records | Desktop `GROKPTAH_HOME` orchestration store | Service `GROKPTAH_HOME` | Service `GROKPTAH_HOME` | Shared service ledger. Same bearer + allowlist ⇒ same Agents |
| Lane / session meta + transcript | Desktop session store | Service session store | Service session store | Shared on the service. A **desktop client** currently projects remote Lanes without pulling transcript (`message_count: 0`) |
| Runs, checkpoints, event journal, audit, idempotency | Desktop orch store | Service orch store | Service orch store | Shared. Reads are session/workspace/run scoped |
| Follow-up queue | Desktop session file | Service session file | Service session file | Shared. Mutations are CAS; conflicts return `stale_version` |
| Admission queue | Desktop process memory | Service process memory | Service process memory | Shared inside one process. Restart interrupts queued Runs |
| Composer drafts, open tabs, layout, theme | That desktop only | Not applicable | Not applicable | **Not synchronized** |
| Provider keys, xAI login, gateway profiles | That desktop only | Not in this contract | Not in this contract | **Not synchronized** |
| Service bearer token | Tauri memory on the connecting desktop | Process env / unit config | Process env / proxy | Each client holds its own copy. UI must never echo it |
| Source files | Desktop filesystem | Service allowlisted paths | Service allowlisted paths | Visible only if that client already has filesystem access to the **same** machine path. **Not synced by GrokPtah** |
| Terminal / PTY | Desktop process | Not exposed over MCP | Not exposed over MCP | **Not synchronized** |
| Clipboard | Local OS | No | No | **Not synchronized** |
| Computer Use grants, screenshots, observations | Desktop computer-use store | Not a service mutation | Not a service mutation | **Not synchronized.** Local authority only |
| Tool permissions (live) | Desktop modal for local Runs | Live events on the service stream | Live events on the service stream | Each connected client can see scoped events; granting is a service-side decision for service-owned Runs |
| Isolated approval / promotion | Desktop orch store | Service orch store | Service orch store | Shared on the service. Fingerprint revalidation at promote time |
| Workspace readiness | Desktop path check | Service path check | Service path check | A desktop client must not report service workspace `ready` because a local folder exists, or `missing` because it does not |

### 4.2 What is and is not synchronized

**Synchronized across clients of one service** (when those clients are authorized for the same allowlist):

- Agent records and `lane_ids` as stored on that service.
- Service-owned session list (id, title, cwd, busy, updated).
- Durable Runs, checkpoints, retained event pages, isolated approvals, promotion state.
- Follow-up queue snapshots and revisions.
- Capacity / admission positions (live, not a reservation).

**Not synchronized, and the UI must not imply otherwise:**

- Credentials and provider profiles.
- Source trees, git working copies, or worktrees.
- Terminal processes, PTY buffers, or local shells.
- Clipboard contents.
- Computer Use authority, screenshots, or grants.
- Desktop chrome (tabs, docks, Live rail, drafts, appearance).
- Local-desktop Lanes that were never created on the service.
- Full transcripts to a remote desktop client (not in the current remote projection).
- Presence of other operators. Protocol tests prove multi-client ledger access; Phase 1 did not observe a second device. Do not ship a “Jane is viewing this Lane” feature from this review.

### 4.3 Exclusive store lock

One `GROKPTAH_HOME` orchestration store, and one computer-use store, may be open in **one** process.

If the desktop and a local service share `~/.grokptah`, the second opener fails with the lock error Phase 1 surfaced. Product recovery:

- Primary: “GrokPtah’s saved work is already open in another window or service.”
- Actions: “Bring that window forward,” “Stop the local service,” or “Use a separate data directory.”
- Technical details: store path and OS error.

Do not present this as “No Agents yet.”

### 4.4 Multi-client conflict language

When a second client wins a queue CAS:

- Primary: “This Lane’s queue changed. Refresh to see the current order.”
- Not: `stale_version`, revision integers, or entry ids.

When a second client submits while a Lane is busy and `allow_queue` is false:

- Primary: “This Lane is already working. Queue the follow-up or wait.”

When two clients watch the same running service Run:

- Each client’s disconnect leaves the Run running.
- Each client recovers by reconnect + cursor catch-up, or by reading durable progress if the cursor expired.

---

## 5. User-visible behavior

Copy below is the required primary language. Technical details are optional disclosure.

### Missing workspace

- **Show:** “This Lane’s project folder is unavailable. Tools and model turns are paused.”
- **Action:** “Choose folder.”
- **Do not:** run tools, send turns, or show a simultaneous empty Tools state as if nothing was ever built.
- **Keep:** transcript, Runs, Agent binding.
- Distinguish `missing`, `not_directory`, and `inaccessible` only in the supporting sentence or details.

### Disconnected service

- **Show:** “Runtime is disconnected. This Lane’s history is still saved on the service. Live controls are paused.”
- **Action:** “Reconnect” and, if a local Lane exists separately, “Work locally in another Lane.”
- **Do not:** change Runtime to Local desktop; do not say the Run stopped solely because this window disconnected.

### Reconnecting or stale event stream

- **Show:** “Reconnecting. Showing last known progress.” Mark the progress as stale.
- **Action:** none required beyond waiting; offer “Refresh” after a bounded failure.
- **Do not:** replace the last-known Run with an empty state.

### Expired event cursor

- **Show:** “Older live events are no longer kept. Progress, changes, tests, and the handoff are still available.”
- **Action:** “View progress” / “View changes.”
- **Do not:** silently continue from a newer cursor; do not dump `after_seq`.

### Queued work

Distinguish the two queues in the supporting sentence.

- Admission: “Waiting for a free slot. Position 2 of 4.” Action: “Cancel queued Run.”
- Follow-up: “2 follow-ups waiting in this Lane.” Action: “Edit queue.”
- **Do not:** open a live event stream as if the Run had started.

### Active Run

- **Show:** “Working in this Lane.” Agent, Runtime, round/tool if known.
- **Actions:** View progress, Steer, Cancel.
- Steering copy: “Guidance applies at the next safe step.”

### Awaiting approval

Name which approval.

- Isolated promote: “Review required before applying these files to the project.” Actions: Review diff, Approve, Discard. Bind copy to file count and expiry.
- Tool permission: “[Tool] needs permission in this Lane.” Actions: Allow, Deny.
- Computer Use: “Computer Run needs a local grant.” Never use this copy for a hosted Lane.

Expired isolated approval: “Approval expired. Review the same changes again.”

### Failed Run

- **Show:** “This Run failed. The Lane and earlier checkpoints are intact.”
- **Actions:** Inspect evidence; Start new Run; if a verified checkpoint exists, Resume from checkpoint.
- **Do not:** offer Retry unless the durable state is `interrupted`.

### Interrupted Run

- **Show:** “Work stopped before it finished. A replacement Run will not start until you choose one.”
- **Actions:** Inspect; Retry this Run (fresh prompt, same mode); if a checkpoint exists, Resume from checkpoint; Start new.
- **Do not:** auto-retry.

### Verified checkpoint

- **Show:** “Verified continue point from [when / which Run]. Resume starts a new Run and keeps this history.”
- **Action:** “Resume from checkpoint…” (requires prompt).
- Tamper/mismatch: “This continue point cannot be used. Inspect history or start a new Run.”

### Retry, resume, or start new

Present as three named choices whenever more than one is legal. If only one is legal, do not show the others as disabled peers without a one-line reason (“Retry is only for interrupted Runs”).

### Archived Lane

- **Show:** “This Lane is archived. Its history is kept.”
- **Actions:** Inspect (read-only), Restore.
- **Do not:** Unarchive as a side effect of inspect or search-open.
- **Do not:** say deleted.

### Restored Lane

- **Show:** the re-derived active status and the same history.
- **Next:** Ready, Interrupted, Failed, or Workspace unavailable — never a blank new session.

### Paused Agent

- **Show:** “This Agent is paused. You can inspect its Lanes. New work needs Unpause.”
- **Do not:** archive its Lanes.

### Retired Agent

- **Show:** “This Agent is retired. Historical Lanes and Runs are kept. It cannot start new work.”
- **Actions:** Inspect; Unretire if allowed.
- **Do not:** offer Start Lane from that Agent.

### Empty vs error vs loading

| Situation | Title | Action |
|---|---|---|
| Agents loaded, zero records | No durable Agents yet | Create Agent or start an ad-hoc Lane |
| Agent refresh failed | Couldn’t load Agents | Retry. Last-known list if any, else error only |
| No active Lanes, archive non-empty | No active Lanes | Start Lane / Browse archive (N) |
| No Lanes at all | No Lanes yet | Start Lane |
| Lane list failed | Couldn’t load Lanes | Retry |
| No Runs, refresh ok | No durable Runs yet | Submit a Build prompt |
| Runs refresh failed | Couldn’t load Runs | Retry; keep last-known Runs |

---

## 6. Shared terminology grammar

### 6.1 Preferred words

| Say | Do not say in primary UI |
|---|---|
| Agent | Persistent agent, durable identity, session agent |
| Lane | Session, build, zone, tab (tab is only a view) |
| Ad hoc | Unassigned Agent, missing Agent, no identity (as an error) |
| Run | Task, turn, job (turn may appear in details) |
| Runtime | Endpoint, MCP, bridge, remote target |
| Local desktop / Local service / Hosted service | Local, cloud, remote, VM (unless naming a specific machine) |
| Workspace / project folder | cwd, path, allowlist (allowlist may appear in details) |
| Archive / Restore | Hide, unarchive, bury |
| Retire | Archive Agent, delete Agent |
| Pause / Unpause | Disable, mute |
| Resume from checkpoint | Resume (bare), continue, reload history |
| Retry interrupted Run | Resume, restart |
| Start new Run | Resume, try again (ambiguous) |
| Reconnect | Resume, refresh (refresh is for a failed load) |
| Needs review | Permission, approval (until the kind is named) |
| Working | Busy, running (running is acceptable as a badge if Working is the sentence) |
| Follow-up queue | Queue (bare) |
| Waiting for a slot | Queue (bare) |
| Technical details | Dumping the raw error as the title |

### 6.2 Detail-only words

`session_id`, `lane_id`, `agent-{uuid}`, `GROKPTAH_HOME`, store paths, `os error 35`, bridge version, `cursor`, `after_seq`, `Last-Event-ID`, `stale_version`, `request_id`, bearer token, `npx`, stdio, probe, `.tmp*`, worktree paths under `.grokptah/worktrees/`.

### 6.3 Computer terms

| Term | Meaning |
|---|---|
| Computer Use | Local observation/action capability (#273) |
| Computer Run | One bounded local Computer Use execution |
| Simulator | Offline test double |
| Grant | Local, non-transferable authority for that Computer Run |

### 6.4 Sentence shape

`[What is true for the user]. [What is paused or preserved]. [One next action].`

Example: “Runtime is disconnected. History is still on the service. Reconnect to watch live progress.”

---

## 7. Migration plan

### 7.1 Compatibility invariants

Do not break, rename away, or delete in the first slices:

- `session_id` as the durable transcript and MCP scope key.
- `SessionSummary` field names consumed by desktop chrome, search, Browse, and `workspace.json`.
- `AgentRecord.session_id` as a readable legacy field.
- `RunRecord.session_id`, `retry_of`, `parent_run_id`, approval fingerprints.
- Checkpoint hash validation.
- MCP tool names and fail-closed authorization.
- Exclusive store locks.
- No implicit resume.

### 7.2 Mapping from `SessionSummary` / `session_id`

| Legacy | Product | Migration rule |
|---|---|---|
| `SessionSummary.id` | `lane_id` and `transcript_session_id` | Equal in slice 1. Never generate a second id |
| `SessionSummary.agent_id` | Current Agent owner | Optional. Null ⇒ Ad hoc |
| `SessionSummary.title` | Lane title | Keep. Do not replace with path |
| `SessionSummary.cwd` | Workspace reference | Keep. Status is computed |
| `SessionSummary.kind` | Lane kind | `chat` stays a Lane; cannot attach an Agent |
| `SessionSummary.archived` | Archive flag | Keep |
| `SessionSummary.forked_from` | Fork lineage | Keep |
| `SessionSummary.execution_mode` | Default for **next** Run | Keep |
| `workspace.json` `active_session` | Focused Lane | `active_lane_id` is an alias, not a second persisted field |
| `AgentRecord.session_id` | Primary resume Lane | Required until slice “Unbind resume” |
| `AgentRecord.lane_ids` | Known Lanes | Normalize empty → `[session_id]` on read |
| `RunRecord.session_id` | `lane_id()` | Derived accessor; no durable rename yet |
| Desktop “Resume (load history)” | Open Lane / load transcript | Rename in UI; it is not checkpoint resume |
| Composer “Run on” remote | Submit to a **different** Lane | Label as such until true retarget exists |

### 7.3 Staged sequence

PR #309 already shipped the italicized work. Later slices must not redo it.

1. **Projection (done).** `LaneSummary` over `SessionSummary`; `lane_id = session_id`.
2. **Explicit UI scope (partially done).** Finish remaining Tools, terminal, Git, MCP, Computer Run, and composer so none infer ownership from global focus. Acceptance: two open Lanes cannot show each other’s diffs, queue, or composer target.
3. **Normalized state grammar (partially done).** Adopt `StateCard` everywhere Phase 1 showed empty+error. Map store-lock, disconnect, cursor expiry, and workspace status to the copy in §5.
4. **Honest Runtime projection (partially done).** Keep local/service/hosted badges. Stop implying remote Lanes have local transcripts, archive state, or workspace checks. Stop implying “Run on” retargets the focused Lane.
5. **Archive semantics correction.** Inspect-without-restore. Block new work while archived (UI + `require_build_session`). Bulk archive is Lane-only.
6. **Unbind resume from primary `session_id` (blocker for the one-Agent-many-Lanes story).**  
   Resume validates: Agent owns the requested Lane (`lane_ids` / `known_lane_ids`), checkpoint belongs to that Agent, checkpoint workspace matches the requested Lane workspace, Agent `can_resume`.  
   `AgentRecord.session_id` remains serialized for old records and becomes “last resume Lane,” not the authorization key.  
   Checkpoints stay Lane-attributed via their existing `session_id`.  
   This is the #297 overlap; Phase 2 UI must not lie before this ships.
7. **Agent lifecycle.** Add `paused` / `retired` as a **separate field** from operational `AgentState`. Do not overload `AgentState`. Gate new work. Do not auto-archive Lanes.
8. **Assignment UX.** “Assign Agent” on ad-hoc Build Lanes using `attach_session_to_agent`. No Agent-to-Agent transfer until history records exist.
9. **Visual IA slices** (only after 2–6). Agent roster, Agent detail, Lane list/archive, focused Lane workspace. Independently reviewable PRs. No application-wide rewrite.
10. **Hosted multi-device research pass.** A real second client against `grokptah-service`. Until then, hosted copy stays contract-faithful and research-bounded.

### 7.4 Explicit non-goals for this migration

- New persisted Lane table.
- Automatic Agent creation on every Build turn becoming a user-facing “you now have an Agent” surprise without copy (today `ensure_session_agent` can mint `agent-{session_id}` — hide or explain it).
- Automatic resume, scheduler, or unattended Agent.
- Synchronizing secrets, files, terminals, clipboard, or Computer Use.
- Deleting `session_id` from MCP or transcripts.
- Treating Chat Lanes as Agents.

---

## 8. Acceptance criteria

A Phase 2 design or a later implementation slice is acceptable only if a reviewer can verify the following without a protocol manual.

### 8.1 Identity and ownership

- [ ] The focused object is named as a Lane, with Agent or Ad hoc, Runtime, workspace, and current Run visible.
- [ ] One Agent detail shows multiple active and archived Lanes without implying they are one session.
- [ ] An ad-hoc Lane does not look broken.
- [ ] Composer, terminal, Tools, Git, diffs, tests, queue, steering, approvals, MCP, and Computer Run all display the same Lane id as the surface that will mutate.
- [ ] Two open Lanes cannot operate on each other’s workspace or queue.

### 8.2 Lifecycle

- [ ] Archive copy states that history is kept and Restore is the reverse.
- [ ] Inspecting Archived does not restore.
- [ ] Restore does not start a Run.
- [ ] Retire copy does not mention deleting or archiving Lanes.
- [ ] Pause and Retire are different actions with different confirmations.
- [ ] Resume from checkpoint, Retry interrupted Run, Start new Run, and Reconnect are distinct and only offered when legal.

### 8.3 Runtime honesty

- [ ] Local desktop, local service, and hosted service are distinguishable before prompt submit.
- [ ] Disconnect does not relabel a hosted Lane as local.
- [ ] Copy never claims credentials, source files, terminals, clipboard, or Computer Use grants sync.
- [ ] Remote projection does not invent transcript counts, archive state, or local workspace readiness.
- [ ] “Run on” names the service Lane that will execute.

### 8.4 State integrity

- [ ] Empty, loading, error, and stale cannot contradict one another on the same collection.
- [ ] Store-lock failures use the recovery copy in §4.3, with path/OS code under Technical details.
- [ ] Cursor expiry uses the copy in §5, not a raw `cursor_expired`.
- [ ] Missing workspace blocks execution and offers Choose folder.
- [ ] Failed Run does not offer Retry unless the record is `interrupted`.

### 8.5 Migration safety

- [ ] Existing `SessionSummary` rows appear as Lanes with stable ids.
- [ ] Legacy Agents without `lane_ids` still expose their primary session as a Lane.
- [ ] MCP `session_id` scope still authorizes Runs.
- [ ] No implicit resume after restart.
- [ ] Production UI rewrite is not required to accept this contract.

### 8.6 Computer Use

- [ ] Computer Run is Lane-scoped and local-only.
- [ ] #273 states (`operator_takeover`, grant expiry, interrupted ≠ paused) are not flattened into Agent Pause.

### 8.7 Prototype-only (issue #308 Phase 2)

- [ ] At least two directions are scored with `SCENARIO-RUBRIC.md` S01–S13.
- [ ] S04 and S08 are scored as contract reviews, not observed hosted successes.
- [ ] Hard gates in the rubric all pass for any recommended direction.

---

## 9. Evidence gaps and unresolved decisions

### 9.1 Contradictions in the current design package

| ID | Conflict | Resolution this review adopts |
|---|---|---|
| C1 | Design Agent lifecycle is `active \| paused \| retired`. Code `AgentState` is `active \| waiting \| interrupted \| failed \| completed` | Treat code as operational health. Lifecycle is a new field, unimplemented |
| C2 | Design `runtime_connection` includes `stale` / `unknown`; code has `reconnecting` / `error` | Do not add wire values. `stale` is presentation |
| C3 | Design says one Agent owns many Lanes and can resume. Resume still requires primary `session_id` + primary workspace | UI must not promise cross-Lane resume until slice 6 |
| C4 | Design archive “prevents new work.” Code only hides the Lane and Browse auto-unarchives | Change product + control plane; current behavior is a defect |
| C5 | Design Runtime selector is “this Lane’s target.” Composer “Run on” is another session | Call it a different Lane until a real retarget exists |
| C6 | Design Lane `status` looks stored. Only archive + workspace + Run/Agent states exist | Derive status (§3.2) |
| C7 | Fable / handoff ask for Agent home on multiple devices. Phase 1 never saw hosted or second-device use | Hosted multi-device is a contract + future research pass |
| C8 | Issue #308 asks IA to distinguish workload and routine. Those objects are #305/#297 | Placeholder only |
| C9 | Persistent Agent panel still says “Complete a Build turn to create one,” which can mint `agent-{session_id}` without a user-facing Create Agent | Creating an Agent must become explicit before Agents are a top-level IA object |
| C10 | Context bar can say “Ad hoc Agent” | Forbidden by this grammar |
| C11 | Computer Use `paused` vs Agent Pause | Different objects; qualify the noun |
| C12 | Two queues and three approvals are one word each in much of the UX writing | Split per §1.4 |
| C13 | `projectRemoteSessionAsLane` forces `workspace_status: ready` and `archived: false` | Honest unknown: do not display local workspace/archive claims for service Lanes |
| C14 | `session_delete` leaves Agent/Run rows | Confirmation must not say “everything is gone” |
| C15 | Design health includes `needs_attention`; no such Agent enum exists | Derive Attention from Lane derivation + pending review, do not store it on Agent |

### 9.2 Unsupported assumptions to drop

- That hosted GrokPtah already has a user-facing “home” reachable from any device with synchronized work.
- That connecting a desktop to a service moves or mirrors the local transcript.
- That Local service and the desktop can share one `GROKPTAH_HOME` concurrently.
- That Pause/Retire already exist and only need chrome.
- That Fork keeps the Agent.
- That Resume (load history) is checkpoint resume.
- That Computer Use will be available on hosted Lanes in this phase.
- That empty archive in Phase 1 says anything about restore quality (restore was not exercised).

### 9.3 Evidence gaps

| Gap | Why it matters | What would close it |
|---|---|---|
| No hosted E2E walkthrough | Cannot validate S04 copy against real latency, auth failure, or allowlist errors | Supervised service + desktop connect, with screenshots |
| No second-client observation | Multi-client tests prove ledger isolation, not operator UX for conflicts | Two desktops, one token, queue CAS + live Run |
| No reconnect-after-sleep/network-loss on desktop | Reconnecting/stale copy is inferred from service tests | Force-drop SSE while a Run is live |
| No live isolated promote / deny in Phase 1 | Approval copy is from code + docs | Exercise review → approve → promote and expiry |
| No interrupt → retry in Phase 1 | Interrupted copy is from store-reopen tests | Kill process mid-run, reopen, retry |
| Narrow/keyboard pass missing | Cannot accept D17 | Prototype + later implementation at a narrow width |
| Agent display name / role / policy absent | Roster will show raw `agent_id` | #297 Agent spec |
| Memory scopes absent | Do not show “memory synced” | #298 / #297 |
| Workload/routine absent | Do not design them as Lanes | #305 |
| PR #309 CI failing (per integration ledger) | Foundation is not yet a mergeable baseline | Green CI or documented non-code limitation |
| Store-lock owner | User cannot see *which* process holds the lock | Future diagnostic: pid/name in Technical details only |

### 9.4 Decision dispositions (contract scope)

Statuses use the vocabulary in `OPEN-DECISIONS.md`.

| ID | Topic | Disposition |
|---|---|---|
| D01 | Default landing | Prototype and test. Contract only requires the empty/error/archive cases |
| D02 | Agents and Lanes both top-level | **Decided:** both permanently reachable |
| D03 | Multi-Lane supervision | Prototype and test, with per-zone ownership mandatory |
| D04 | Assign Agent after create | **Contract:** yes, ad-hoc Build → Agent, explicit, non-rewriting. No routine reassignment |
| D05 | Retire qualification | **Contract:** block on queued/running Runs and live isolated approvals. Do not auto-archive Lanes |
| D06 | Bulk archive | **Decided:** Lanes only; never bulk retire |
| D07 | Human-readable Lane id | **Decided:** title + Agent/Ad hoc + project display name + status. Paths/ids in details |
| D08 | Where Runtime is selected | Prototype and test. Must be visible before submit |
| D09 | Hosted Agent home | **Deferred** with boundary: no proven cross-device home |
| D10 | Cross-device sync | **Contract:** §4.2 is authoritative |
| D11 | Change Runtime after create | **Contract:** not a silent retarget; different Lane or explicit continue-elsewhere |
| D12 | Focused Lane default content | Prototype and test |
| D13 | Attention order | **Decided for prototypes:** review → interrupted → workspace/runtime repair → failed → queued → working → completed/archive-ready |
| D14 | Technical detail visibility | **Decided:** impact + next action first; diagnostics under Technical details |
| D15 | Meaning of Resume | **Contract:** §2.6 is authoritative |
| D16 | Contextual tool persistence | **Decided:** Lane/Runtime/workspace/Run/composer survive opening any drawer |
| D17 | Narrow drawers | Prototype and test |
| D18 | Expert Grid scope | **Decided:** every zone owns its Lane; focus is not authority |
| D19 | Visual identity | Prototype and test |
| D20 | First implementation slice | **Decided after this contract:** finish explicit Lane scope + state grammar + archive correction **before** visual IA. Unbind resume before Agent-home claims many-Lane continuation |

### 9.5 What this review is not

- Not a prototype direction score.
- Not permission to implement production UI.
- Not a claim that PR #309 is merge-ready.
- Not a hosted or Computer Use success report.

The next useful artifacts are (1) prototypes that obey this contract, scored with `SCENARIO-RUBRIC.md`, and (2) the resume-unbind / archive-enforcement implementation slices once a direction is chosen.
