# Phase 2 implementation roadmap

This roadmap converts the selected Phase 2 hybrid into independently
reviewable vertical slices. It is a design handoff, not authorization to change
production UI in this branch.

Each slice requires before/after visual evidence, interaction tests,
accessibility checks, and regression coverage. A later slice may not make an
earlier ownership or lifecycle rule less explicit.

## Contract prerequisites

Before visual surfaces promise the related behavior:

1. Scope every contextual query and mutation by explicit `lane_id`.
2. Separate archived inspection from restore and reject new archived mutation.
3. Preserve remote “Run on” as a separate service-owned Lane; do not silently
   retarget a Lane.
4. Remove or explicitly expose the primary-session/workspace checkpoint-resume
   restriction before promising cross-Lane Agent continuation.
5. Add a distinct Agent lifecycle field and mutation gates before Pause,
   Retire, or Unretire ship without a “Proposed” label.
6. Keep D04 assignment Build-only and non-rewriting; expose the primary-resume
   restriction and do not add Agent-to-Agent reassignment before #297 provides
   attributable history and cross-Lane checkpoint validation.

## Vertical slices

| Slice | User-visible outcome | Visual acceptance | Interaction acceptance | Accessibility acceptance | Regression acceptance |
| --- | --- | --- | --- | --- | --- |
| 1. Shared state and scope grammar | Existing UI uses one vocabulary for loading, empty, error, stale, disconnected, blocked, interrupted, queued, approval, archived, and retired/proposed states. | Screenshot matrix shows one primary state and one next action; technical diagnostics are subordinate. | Every composer, tool, approval, terminal, and evidence request names and sends an explicit Lane id. | Icon + text for every state; visible focus; errors announced without replacing recoverable content incorrectly. | Existing queue, steering, diff, tests, terminal, MCP, archive, and retry operations remain reachable and Lane-correct. |
| 2. Focused Lane Workbench | D1 becomes the default work surface with context header, transcript, one composer, and contextual drawers. | Desktop and 760px before/after captures match the selected hierarchy; no body-level horizontal scroll. | Drawer open/close returns focus; composer target is visible before send; blocked reasons match the banner. | Heading focus does not move the viewport; dock controls retain accessible names; Escape closes overlays; reduced motion respected. | Session compatibility projection, prompt send, queue, fork/rewind equivalents, terminal, and file/diff flows pass. |
| 3. Lanes library and archive | Active, Attention, Archived, and All views replace anonymous inventory without losing search/history. | Rows retain title, Agent/Ad hoc, workspace display name, Runtime, state, activity, and evidence indicators. | Search preserves context; Inspect Archived never restores; Restore is explicit; bulk archive affects Lanes only. | Search result count is announced; row actions have object-specific names; narrow rows stack without truncating the next action. | Archive remains reversible; transcripts, Runs, checkpoints, approvals, evidence, attribution, and workspace metadata survive. |
| 4. Agents identity spine | D2 roster/detail becomes the permanent Agents destination after Agent↔Lane contracts are honest. | Lifecycle and health are separate; Runtime appears only as current/last Lane or aggregate; active and archived Lane groups remain visible. | Start Lane chooses Runtime before first prompt; retirement blocks on live work and preserves history; D04 assigns only ad-hoc Build Lanes, states the primary-resume limitation, and offers no routine reassignment. | Card and group headings expose structure; lifecycle consequences are read before confirmation; no color-only health. | Agent memory/policy/checkpoint data, ad-hoc Lanes, and existing Lane attribution remain intact through migration. |
| 5. Runtime and hosted clarity | Runtime screen and Lane headers expose desktop, local service/VM, and hosted ownership honestly. | “What persists/syncs” copy distinguishes local GrokPtah data from provider requests and service-owned ledgers. | Reconnect, Resume, Retry, and Continue in another Lane are separate; credentials/files/terminals/Computer Use authority never appear moved. | Connection transitions have text and restrained live announcements; diagnostics stay behind disclosure. | Protocol conformance, durable cursor reconnect, separate remote Lane creation, and disconnected-history reads pass. |
| 6. Expert supervision mode | D3 pinned-Inspector workspace ships as opt-in multi-Lane supervision. | Two self-labelled zones and pinned Inspector; focused exit always visible; narrow layout stacks zones then Inspector. | Inspector changes only through its pin control; every zone has its own composer; Focus preserves Lane context and queue position. | Predictable zone/Inspector heading order; keyboard pin control; no focus-following mutation; stacked narrow order matches reading order. | Simultaneous queue, approval, diff/test, and focused transitions remain scoped; shared components do not diverge from D1. |
| 7. Computer Use integration (#273) | Lane-scoped operator controls appear only when capability, permission, target, and fresh evidence contracts are satisfied. | Target, Lane, Run, grant, provider/model, origin, budgets, current action, freshness, approval binding, and invalidation are visible; controls never clip. | Pause, Stop, Take over, and non-cancelling Steer remain reachable; target/restart/staleness invalidates authority; reload requires new authorization. | Controls have stable names and focus order; current target remains visible at narrow widths; secrets never enter retained evidence or announcements. | Unsupported, revoked, lost-target, stale, sensitive-surface, interrupted, limit, expiry, and restart tests fail closed with durable evidence preserved. |

## Review evidence required per slice

Every implementation pull request should include:

- the exact Phase 2 prototype route or component it implements;
- desktop, 760px, and relevant light/dark screenshots;
- a short task walkthrough for the affected S01–S14 scenarios;
- keyboard and screen-reader notes, including focus return and announcements;
- tests for Lane ownership, lifecycle consequences, persistence, and recovery;
- a list of preserved expert capabilities and any intentionally deferred state;
- an explicit statement when a hosted or Computer Use frame is contract-only
  rather than observed end-to-end evidence.

## Suggested first implementation pull request

Begin with Slice 1: shared state and Lane-scope grammar inside the existing
shell. It provides immediate safety and clarity, is independently reviewable,
and does not require the Agent data-model or navigation migration to land at
the same time.
