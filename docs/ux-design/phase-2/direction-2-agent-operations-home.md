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
                           runtime + connection          Lanes grouped:
                           active/archived counts          Attention needed
                           current Lane → link             Active
                           checkpoint (or "none yet")      Archived (collapsed)
                           Open / Pause / Retire…        Adopt ad-hoc work
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
- **Retire lives here, and only here.** Retire is an Agent-level action with
  consequence copy (blocks new work; preserves memory, checkpoints, and all
  historical Lanes; names how many active Lanes would be blocked). Archive
  never appears on an Agent; Retire never appears on a Lane.
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
3. **Ad-hoc adoption is explicit and history-preserving.** Ad-hoc Lanes
   appear on the roster ("Ad-hoc work (no Agent)") and on each Agent detail
   as adoptable; the Assign dialog states that the Lane's transcript and
   Runs are unchanged and that policy/memory apply from the next Run onward.
4. **Attention rail.** The left rail lists Lanes needing a decision
   (approval, interruption, blockage) across all Agents, so identity-first
   never hides urgency.
5. **Retired is a readable past, not a hole.** `#/d2/agent/agent-4` shows
   the retired banner, blocked Start-Lane form with reason, and the archived
   Lane history intact; its archived Lane (`#/d2/lane/lane-8`) blocks the
   composer with "reassign or unretire" guidance.

## How the audit findings are applied

| Finding | Application in D2 |
|---|---|
| F-01 | Roster/detail use the shared one-state-per-view cards; the audited lock error becomes Retry + Technical details, replacing (not joining) the empty state |
| F-02 | Answered structurally: Agents are home |
| F-03 | Lane surfaces shared with D1; every drawer scope-labeled |
| F-04 | Home is a readable roster of ~cards, not a cockpit; work surfaces open one Lane |
| F-05 | Pause/Retire on Agents with consequence copy; Archive/Restore on Lanes; Delete nowhere near either |
| F-06 | Computer Use unchanged from D1 (single drawer, audited state) |
| F-07 | Runtime chip on every card, in Start Lane, in Lane header, and a Runtime screen with sync boundaries |
| F-08 | Lane search returns identity-bearing rows (title, Agent, workspace, state, last activity) |
| F-10 | Same shared grammar |
| Fable 6 (.tmp noise) | Scratch Lane displays "Scratch workspace" with the `.tmp` path behind Technical details |

## Strengths

- The product model teaches itself: opening the app answers "who works for
  me, what are they doing, what needs me" — the operations questions the
  handoff's acceptance criteria ask.
- Retire vs Archive separation is structural, not just copy — they live on
  different object types with different confirmation dialogs.
- Best fit for hosted/service operation: connection state, checkpoint
  recency, and needs-attention roll up naturally per identity.
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

1. `AgentRecord.session_id` one-to-one must become Agent→Lane association
   (runtime-model migration step 2) before the roster can honestly show
   counts and current-Lane pointers.
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
