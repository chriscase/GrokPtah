# Direction 3 — Adaptive Expert Workspace

Prototype: open `prototype/index.html#/d3/workspace`
Tagline: *multi-Lane supervision with scope made explicit everywhere.*

## Thesis

The current cockpit exists because real operators supervise several work
streams at once. This direction keeps that power as the default and fixes
what the audit actually indicted: not multi-Lane viewing itself, but
**implicit scope** — panels that appear to belong to whatever is focused,
vocabulary about zones/docking, and density with no gradient.

The workspace shows one to three Lane zones plus a right-hand **Inspector**.
Every zone carries its own full context header and its own composer. The
Inspector is **pinned**, not focus-following: it names the Lane it shows,
and changing zone focus never changes it. A scope bar above everything
states exactly what is on screen.

## Hierarchy

```text
Scope bar: "Viewing 2 Lanes. Every zone and panel names its own Lane —
            nothing follows tab focus."            [Exit to focused Lane]

Rail (dense, informative)     Zones (1–3)               Inspector (pinned)
─────────────────────────     ────────────────────      ─────────────────────
Workspace / Runtime           ZONE A  <lane title>      "Pinned to: <select>"
AGENTS (all 4, with             context header          "Showing <Lane> only.
  lifecycle · health)           state banner             Changing zone focus
LANES (all active, with         compact transcript       never changes this
  Agent · state)                composer (explicit       panel."
Archive…                          target)               Tabs: Queue & steering /
                              ZONE B  <lane title>        Changes & tests /
                                …                         Approvals / Run history
```

- **Scope bar as contract.** It is the standing answer to "what am I
  looking at" and the exit ramp to a single focused Lane (the same surface
  as Directions 1/2 — supervision and focus are two zoom levels of one
  product, not two products).
- **Zones are self-sufficient.** Each zone's header carries Lane, Agent,
  Runtime + connection, Workspace, and Run; each composer states its target.
  There is no shared composer and no "which zone owns the send" question.
- **The Inspector replaces the ambient Tools panel.** It is one panel with
  an explicit pin selector. Its scope line is always visible. Tabs cover the
  supervision set (queue/steering, changes/tests, approvals, run history);
  the remaining drawers (terminal, MCP, Computer Use) are reached by
  focusing the Lane — deliberate friction that keeps mutation-adjacent
  surfaces on a single-Lane screen.
- **The rail is a legend, not a junk drawer.** Agents (with lifecycle ·
  health) and active Lanes (with Agent · state) are separate sections with
  distinct iconography; Archive is one link away; no `.tmp` names, no
  zero-message ghosts.

## Key interactions

1. **Pin, don't infer.** The Inspector's pin selector is the only way its
   subject changes. This is the structural fix for the audit's
   wrong-lane-Tools observation (F-03): the failure mode is made
   unrepresentable rather than discouraged.
2. **Zoom levels.** Supervision (2–3 zones) ⇄ Focus (one Lane, full drawers)
   via per-zone "Focus" buttons and the scope-bar exit. State, scroll, and
   queue positions persist across the transition.
3. **Attention still interrupts correctly.** Zone banners render the same
   single-next-action grammar; an approval in Zone B shows its purple banner
   in the zone *and* a badge in the Inspector's Approvals tab when pinned
   there.
4. **Density with a gradient.** One zone = identical to Direction 1's
   focused Lane. Two zones = side-by-side. Three (future) = the cap;
   anything more belongs to the roster/list screens, which this direction
   shares with Direction 2.

## How the audit findings are applied

| Finding | Application in D3 |
|---|---|
| F-03 zone ownership | Structural: per-zone headers/composers; pinned Inspector with permanent scope line; no focus-inference anywhere |
| F-04 density | Density is opt-in per zone count; chrome per zone is one header + one banner max; rail is typed and truncation-free |
| F-02 | Agents hold the top rail section with lifecycle · health; Lanes named with owners |
| F-05 | Lifecycle actions live on roster/detail/list screens (shared with D2), not scattered across zone chrome |
| F-06 / F-09 | Terminal/MCP/Computer Use require Focus mode — expert surfaces exist but demand explicit single-Lane context |
| F-07 | Runtime chips per zone header; Runtime screen one rail click away |
| F-01 / F-10 | Same shared state grammar in zones, rail dots, and Inspector |

## Strengths

- Preserves today's real expert workflows (watch a build while reviewing
  another Lane's diff) with strictly less ambiguity than the current UI.
- The pinned Inspector is a genuinely better tool than per-Lane drawers for
  cross-Lane triage: pin the approvals of the Lane you're gatekeeping while
  conversing in another.
- Smallest conceptual retraining for existing power users; "zones" survive,
  but with names, owners, and composers instead of docking vocabulary.
- Degrades gracefully: at narrow widths zones stack and the Inspector moves
  below, each still self-labeled.

## Risks

- **Still the densest default.** A new user landing on two zones plus an
  Inspector will experience a milder version of the audited overload. The
  scope bar explains, but explanation is a tax. (If this direction wins, a
  first-run default of one zone is strongly advised.)
- **Two homes for contextual surfaces** (Inspector tabs vs Focus-mode
  drawers) must stay behaviorally identical or they become the new
  duplicate-controls problem the audit flagged.
- More chrome per screen means the shared grammar must be applied with
  discipline; any bespoke zone state would re-fragment the language.
- Composer proliferation: two visible composers reintroduce a
  "which box was I typing in" risk, mitigated by target lines and distinct
  zone borders, but real.

## Migration implications

1. Depends on the same Lane projection and Lane-scoped panel contract as
   D1/D2; additionally requires zone state (list of open Lane ids + pinned
   Inspector Lane) to become explicit, serializable UI state.
2. The existing multi-zone/docking implementation is the starting point;
   work is mostly *renaming and constraining*: kill focus-inference, attach
   `lane_id` to every panel request, replace dock-capacity vocabulary with
   zone headers.
3. Live rail dissolves into the rail's Lanes section (state dots + names);
   the separate Live concept is retired.
4. Shares roster/detail/list/lane components with D1/D2, so it can ship
   *after* either as an additive mode rather than a fork.

## Open questions for review

- Should the Inspector offer Terminal/MCP tabs behind an "expert surfaces"
  toggle, or is Focus-mode-only the right permanent friction?
- Zone cap: 2 now, 3 later, or 3 now? (Prototype ships 2.)
- Should pinning follow "last Lane I acted on" as an *opt-in* convenience,
  or is any automatic pin movement a violation of the direction's core rule?
