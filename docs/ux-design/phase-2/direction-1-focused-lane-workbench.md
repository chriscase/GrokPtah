# Direction 1 — Focused Lane Workbench

Prototype: open `prototype/index.html#/d1/lane/lane-1`
Tagline: *one calm Lane by default; everything else is a drawer.*

## Thesis

The user's unit of attention is one Lane. The default surface is therefore a
single Lane workspace that looks and behaves like a focused coding
conversation, with every operator capability one drawer away. Multi-Lane
supervision exists, but as an explicit, opt-in **Expert Grid** — the product
never *starts* as a cockpit.

This is the most direct answer to the audit's densest findings: F-04 (default
density), F-03 (ownership across zones), and the Fable review's "the default
cockpit is too dense for ordinary work; an expert Grid view should remain
available without being the default starting point."

## Hierarchy

```text
Rail (left, calm)                Main surface                    Drawer dock (right)
─────────────────                ──────────────────────────      ───────────────────
WORK                             Context header                  Queue & steering
  · up to 5 active Lanes           Lane · Agent · Runtime ·      Changes & tests
    (title, Agent, state)          Workspace · Run               Approvals
  · All Lanes…                   State banner (when not ready)   Terminal
LIBRARY                          Transcript (primary content)    MCP & tools
  · Agents                       Composer                        Computer Use
  · Archive                        "Sends to <Lane> on <Runtime>" Run history
  · Runtime
─────────────────
Expert Grid (opt-in, rail bottom)
```

- The rail carries **work**, not inventory: at most five active Lanes with
  Agent name and state, then "All Lanes…". The 103-item scroll list is gone
  from the primary surface; the full list lives in the Lanes screen with
  views and search.
- Agents are one click away under Library, using the same roster and detail
  screens as Direction 2 (shared component, different prominence).
- The drawer dock replaces the permanent Tools panel. Exactly one drawer is
  open at a time; each drawer opens with an explicit scope line
  ("Scoped to **Stabilize retry queue** · Ptah Refactorer"). Escape closes
  the drawer and returns focus to its toggle.

## Key interactions

1. **Single composer with explicit target.** The composer header always
   reads "Sends to *Lane* on *Runtime*" with a "Continue on another
   Runtime…" affordance — per contract D11 this is never a silent retarget:
   the dialog states that continuing creates or selects a different Lane and
   that files, terminals, credentials, and in-flight Runs do not move. When
   the Lane cannot execute (archived, missing workspace, disconnected
   runtime, retired Agent), the composer visibly disables itself and states
   the one reason and the one repair — the same copy as the Lane banner.
2. **State banner = single next action.** Blocked, interrupted, awaiting
   approval, queued, archived, disconnected, and stale states render exactly
   one banner above the transcript: what happened, what is preserved, and
   the primary action (Choose folder…, Resume from checkpoint c-77, Review &
   decide, Reconnect, Restore Lane). See `#/d1/lane/lane-5` (missing
   workspace), `#/d1/lane/lane-4` (interrupted + verified checkpoint),
   `#/d1/lane/lane-2` (approval).
3. **Drawers keep expert power one interaction away.** Queue & steering
   (steer the active Run without interrupting; reorder/remove queued
   prompts), Changes & tests (files + observed test evidence), Approvals
   (bound to Run + fingerprint, with Review diff first as the safe default),
   Terminal (labeled with the Lane's workspace path), MCP & tools (trust
   gate first), Computer Use (audited unavailable state by default, plus a
   contract-labelled #273/S14 operator-control illustration behind an
   explicit tab), Run history (lineage: continues / retry-of; checkpoint
   inspect).
4. **Badges, not panels, carry urgency.** The dock shows small counts for
   pending approvals and queued prompts; the rail shows per-Lane state dots
   with text labels. Nothing pulses for attention except the Running state.
5. **Expert Grid is a mode, not the home.** `#/d1/grid` shows two zones,
   each a compact Lane surface with its own context header and composer.
   The scope bar states "Every zone and panel names its own Lane — nothing
   follows tab focus." Exit returns to the focused Lane.

## How the audit findings are applied

| Finding | Application in D1 |
|---|---|
| F-01 contradictory error/empty | Shared state-card renders exactly one state; store-lock error keeps roster area, offers Retry + Technical details |
| F-02 Agents/Lanes not first-class | Both are named rail objects; sessions vocabulary absent |
| F-03 zone ownership | Default has one Lane; Grid zones each carry header + composer; drawers open Lane-scoped |
| F-04 density | Default surface: header, banner, transcript, composer. Nothing else until asked |
| F-05 flat lifecycle actions | Open on the row; Archive behind an explicit confirm stating preservation; Delete not present on rows at all |
| F-06 Computer terminology | One noun, "Computer Use," one drawer; the audited unavailable state with named repair, plus the contract-labelled operator-control tab |
| F-07 runtime visibility | Header chip + composer target line + Runtime screen; runtime is per-Lane, and "Continue on another Runtime…" makes retargeting explicit (D11) |
| F-08 ambiguous search | Lane search filters on title/Agent/workspace and says so |
| F-09 intimidating internals | MCP trust copy is consequence-first; doctor output behind details |
| F-10 state grammar | The shared grammar is the only state vocabulary used |

## Strengths

- Lowest cognitive load of the three; the "what should I do next" question
  is answered by a single banner in every non-ready state.
- Ownership is structurally unambiguous in the default mode (there is only
  one Lane on screen).
- Cheapest to explain and to onboard: it resembles the mental model of one
  focused coding conversation.
- The drawer dock generalizes: a future capability is a new drawer, not a
  new peer panel.

## Risks

- The durable Agent is present (header, rail sublabels) but secondary; users
  who think in fleets will pivot through the Lanes/Agents screens more than
  they'd like. Mitigation: the Attention section and roster are one click
  away and share components with D2.
- Heavy multi-Lane operators must enter Grid mode deliberately; if most real
  usage is supervision, the default is a detour (this is the empirical
  question for the next research pass).
- A five-Lane rail cap needs a sensible eviction/ordering rule (recency of
  activity is proposed) or it becomes a mystery.

## Migration implications

Smallest UI delta path of the three:

1. Lane projection over `SessionSummary` (already planned) feeds the rail
   and Lanes screen; no record changes.
2. The current SessionPane becomes the focused Lane surface; Tools panel
   categories become drawers with an explicit `lane_id` prop (contract step
   "scope every contextual query" must land first).
3. Live rail and multi-zone docking collapse into the Expert Grid mode;
   focus/dock vocabulary is retired from the default path.
4. Sessions sidebar remains available behind a compatibility flag until the
   Lanes screen reaches parity (rename, set-cwd, fork, rewind live in the
   Lane detail's overflow, grouped by consequence).

## Open questions for review

- Should the Expert Grid support 3 zones at launch or cap at 2? (Prototype
  caps at 2; the audit's `3/3` dock never demonstrated comprehension.)
- Is the rail's five-Lane cap right, or should it mirror "lanes with
  attention + last N touched"?
- Where does Chat-kind work live in this direction — as Lanes with a `chat`
  badge, or a separate rail section?
