# Direction 2 — Agent Operations Home

Prototype: open `prototype/index.html#/d2/agents`
Tagline: *durable Agents are the front door; Lanes live under them.*

## Thesis

The product's differentiating promise is durable identity: agents that
persist, remember, checkpoint, and can be trusted with recurring roles. This
direction makes that promise the first thing the user sees. Home is the
**Agent roster**; each Agent detail page shows its Lanes grouped by urgency;
work always carries its owner's name. Ad-hoc work is explicitly supported
and explicitly labeled, never second-guessed.

This is the strongest answer to F-02 ("Agents and Lanes are not first-class
navigation objects") and to the Fable review's first finding ("Durable
Agents are buried instead of presented as a primary, long-lived object").

## Hierarchy

```text
Rail                     Home: Agent roster              Agent detail
────                     ─────────────────────           ─────────────────────────
Agents  (home)           Summary strip: Active /         Identity: role, model,
Lanes                      Paused / Retired /              policy, memory,
Runtime                    Need attention                  checkpoint lineage
ATTENTION                Agent cards:                    Start a new Lane
  (lanes needing           name, role                      objective + runtime
   a decision)             lifecycle ≠ health chips        target chosen up front
                           current-Lane runtime          Lanes grouped:
                             (or aggregate — runtime       Attention needed
                             belongs to Lanes)             Active
                           active/archived counts          Archived (collapsed)
                           current Lane → link           Assign ad-hoc Build work
                           checkpoint (or "none yet")
                           Open / Pause / Retire…
                             (marked Proposed)
                         Ad-hoc work section
```

- **Lifecycle and health are two chips, never one.** "Active ·
  Needs attention" (Release Warden) and "Paused · Waiting" (Docs Curator)
  read as different facts, matching the runtime model's explicit warning not
  to conflate them.
- **The roster is honest about absence.** "No checkpoint yet — the first
  completed Run creates one" is a normal statement, not an error. The
  empty roster ("No durable Agents yet") and the failed load ("Couldn't
  refresh durable Agents") are different screens that can never co-render —
  see `#/d2/agents?demo=empty` vs `#/d2/agents?demo=error`.
- **Retire lives here, and only here — as a proposed contract.** Retire is
  an Agent-level action, visibly marked "Proposed" (the lifecycle enum is
  not implemented today). Its dialog states that everything is preserved —
  nothing archived, deleted, moved, reassigned, or rewritten — and blocks
  confirmation while the Agent has queued/running Runs or live isolated
  approvals (D05). Archive never appears on an Agent; Retire never appears
  on a Lane.
- The focused Lane workspace is the same surface as Direction 1 (shared
  component) reached with a breadcrumb: *Agents › Release Warden › Gateway
  model qualification*.

## Key interactions

1. **One-Agent-to-many-Lanes made visible, not diagrammed.** The card shows
   counts and the current Lane; the detail page groups the Agent's Lanes as
   Attention needed / Active / Archived. The relationship is learned by use.
2. **Start Lane chooses runtime up front.** The Start-a-new-Lane form places
   objective and runtime target (with live connection chips: Local desktop ·
   Connected, Local service/VM · Disconnected, Hosted · Connected) before
   any prompt exists, satisfying "switching Runtime target is visible before
   the prompt is submitted." See `#/d2/agent/agent-2`.
3. **D04 assignment is explicit and history-preserving.** Ad-hoc Build Lanes
   appear on the roster ("Ad-hoc work (no Agent)") and on each Agent detail
   as assignable. The dialog states that current ownership is recorded while
   transcript, Runs, and checkpoints remain unchanged. It also exposes the
   primary-resume Lane/workspace limitation and does not offer routine
   Agent-to-Agent reassignment.
4. **Attention rail.** The left rail lists Lanes needing a decision
   (approval, interruption, blockage) across all Agents, so identity-first
   never hides urgency.
5. **Retired is a readable past, not a hole.** `#/d2/agent/agent-4` shows
   the retired banner (marked proposed), a blocked Start-Lane form with
   reason, "Proposed: unretire…" as explicitly-labelled future behavior, and
   the archived Lane history intact; its archived Lane (`#/d2/lane/lane-8`)
   blocks the composer and states that everything is preserved and that
   Agent-to-Agent reassignment is not a routine action in this phase.

## How the audit findings are applied

| Finding | Application in D2 |
|---|---|
| F-01 | Roster/detail use the shared one-state-per-view cards; the audited lock error becomes Retry + Technical details, replacing (not joining) the empty state |
| F-02 | Answered structurally: Agents are home |
| F-03 | Lane surfaces shared with D1; every drawer scope-labeled |
| F-04 | Home is a readable roster of ~cards, not a cockpit; work surfaces open one Lane |
| F-05 | Pause/Retire on Agents with consequence copy and a visible "Proposed" marker (lifecycle is not implemented); Archive/Restore on Lanes; Delete nowhere near either |
| F-06 | Computer Use unchanged from D1 (audited state default + contract-labelled #273 tab) |
| F-07 | Current-Lane runtime (or aggregate) on cards — runtime belongs to Lanes — plus runtime chosen in Start Lane, shown in the Lane header, and a Runtime screen with sync boundaries |
| F-08 | Lane search returns identity-bearing rows (title, Agent, workspace, state, last activity) |
| F-10 | Same shared grammar |
| Fable 6 (.tmp noise) | Scratch Lane displays "Scratch workspace" with the `.tmp` path behind Technical details |

## Strengths

- The product model teaches itself: opening the app answers "who works for
  me, what are they doing, what needs me" — the operations questions the
  handoff's acceptance criteria ask.
- Retire vs Archive separation is structural, not just copy — they live on
  different object types with different confirmation dialogs.
- Best fit for hosted/service operation: per-Lane connection state,
  checkpoint recency, and derived needs-attention roll up naturally per
  identity. (Roster health honesty additionally depends on the cross-Lane
  resume restriction being lifted — see migration below.)
- Scales with the roster: four agents or forty, the summary strip and
  attention rail keep triage constant-time.

## Risks

- **Indirection for quick work.** "I just want to run a prompt in a folder"
  costs a hop (roster → ad-hoc section → Lane) unless a global quick-start
  is added. Mitigation: "Start ad-hoc Lane" is present on the roster and in
  the empty state, but this remains the direction's tax.
- **Single-agent users see ceremony.** With one Agent, home is a page about
  one card. Mitigation: auto-open the sole Agent's detail as home.
- **Ad-hoc work must never feel punished.** The design labels it plainly;
  if future features (checkpoints, memory) become Agent-only, ad-hoc Lanes
  could rot into a second class — a product-policy risk to watch, not a
  layout flaw.

## Migration implications

Requires the Agent-side contract earlier than the other directions:

1. `AgentRecord.session_id` remains a compatibility/current-context field while
   Agent→Lane associations provide the durable roster relationship. The runtime
   now permits same-source workspace continuation only after validating the
   requested Lane and checkpoint; cross-workspace continuation and routine
   Agent-to-Agent reassignment remain unavailable.
2. Roster health/lifecycle needs the normalized state projection; the
   audited lock error must map to the load-failed card.
3. Lanes list, focused Lane, and drawers are shared with D1 (same
   components, same scoping contract).
4. The legacy Sessions sidebar maps to the Lanes screen; "New session"
   becomes Start Lane (ad hoc) with the same defaults.

## Open questions for review

- Home for a zero-Agent, many-ad-hoc-Lane user: roster with a large ad-hoc
  section, or bounce to Lanes until the first Agent exists?
- Should the attention rail deep-link into the drawer that resolves the
  state (e.g. straight to Approvals), or to the Lane with the banner?
- Does Pause need consequence copy as strong as Retire's (queued work
  behavior when pausing mid-Run)?
