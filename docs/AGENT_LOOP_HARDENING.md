# Agent loop hardening

The durable always-on agent loop has to stay honest when it is driven by a
small, cheap, or local model. Such a model fails in a characteristic way: it
does not crash, it repeats itself. It re-reads the same file, restates the same
plan, or announces that it is waiting for something that is not happening. The
loop keeps turning, tokens keep being spent, and the run looks busy.

This module makes that failure visible instead of expensive. It adds no
authority, relaxes no existing gate, and never resolves an ambiguous outcome on
the agent's behalf.

Policy lives in `crates/codegen/grokptah-agent-bridge/src/orchestration/agent_loop.rs`;
durability lives alongside the existing run ledger in `orchestration/store.rs`.

## Three rules

**Spending is not progress.** Tokens, turns, wall-clock, and tool calls are
costs. Only externally attributable change counts as progress: a file edit, a
recorded test observation, or a genuinely novel observation. A step that burns
budget while repeating an earlier `(observation, action)` pair is classified
`no_op` and reported as `stationary` — never as `progressing`.

**Waiting needs a witness.** A model asserting that it is waiting proves
nothing. A wait is productive only while an external witness advances: a
changed witness digest, or a strictly increasing external attempt counter under
a still-open deadline. Unwitnessed waiting is `stalled_wait`, which is
stationary. This is what separates a build that is genuinely running from a
model that has decided to idle.

**Uncertain is absorbing.** A dispatch whose outcome is unknown is never
retried, never escalated into a retry, and never resolved by handing it to a
stronger model — that is still a retry, because the side effect may already
have landed. It stops the loop and requires a human. This mirrors the
computer-use mutation ledger (`Sending` → `Uncertain`) so the two surfaces
cannot drift apart.

## States

A step is classified as exactly one of:

| Class | Meaning |
| --- | --- |
| `mutation` | Files changed or a test was observed. |
| `novel_observation` | A signature not in the retained window, with no mutation. |
| `productive_wait` | A wait whose external witness advanced. |
| `stalled_wait` | A wait whose external witness did not advance. Stationary. |
| `no_op` | A repeat of a retained signature with no mutation. Stationary. |

The loop's disposition after that step is one of:

| Disposition | May continue | Meaning |
| --- | --- | --- |
| `progressing` | yes | The world changed, or something new was seen. |
| `stationary` | yes | A no-op or stalled wait, still inside the envelope. Not progress. |
| `waiting` | yes | An external witness is still advancing. |
| `needs_attention` | no | Absorbing. Requires a manager-issued grant. |
| `exhausted` | no | A budget dimension ran out. Absorbing. |

`stationary` exists so that a no-op is never reported as progress while it is
still within tolerance. It is a visible state, and it is what the public
projection shows.

Both stopped dispositions are absorbing: admitting another step against a
stopped loop is refused rather than silently overwriting the escalation.

## Attention reasons

Each stop names the exact condition that fired. There is no generic "stuck".

- `stationary_loop` — repeated equivalent observation/action pairs, no change.
- `unwitnessed_wait` — waiting with nothing external to show for it.
- `inert_churn` — a fresh action every turn that never changes anything.
- `wait_timeout` — a witnessed wait that outlived its envelope.
- `uncertain_dispatch` — an unknown outcome. Human-only.
- `budget_exhausted` — a named dimension ran out.

## Policy envelopes

Envelopes are keyed on a **declared** model tier. The tier is an operator
input; it is never inferred from a model name. The loop measures nothing about
model quality and asserts nothing about it. An undeclared tier (`unspecified`)
receives the small envelope, because "unknown" must not buy a larger budget —
while the record still says the tier was never declared.

| Bound | `small` / `unspecified` | `large` |
| --- | --- | --- |
| `max_turns` | 12 | 32 |
| `max_tool_calls` | 48 | 160 |
| `max_tokens` | 120,000 | 400,000 |
| `max_wall_ms` | 300,000 | 900,000 |
| `max_stationary_streak` | 2 | 4 |
| `max_consecutive_waits` | 6 | 12 |
| `max_wait_ms` | 60,000 | 180,000 |
| `max_novel_without_mutation` | 8 | 20 |

These are conservative engineering defaults chosen so a stuck loop is cut short
quickly. **They are not derived from any benchmark, cost, or latency
measurement**, and they make no claim about how any model actually performs.
They are the point at which this system stops paying to find out.

A caller may narrow an envelope and may never widen one, mirroring the
`merge_bounds` discipline already used for `RunBounds`. Narrowing cannot change
the declared tier.

## Escalation and handoff

When a loop stops, it issues an `EscalationTicket` bound to the run and the
exact revision it stopped at. The ticket names:

- the reason, verbatim;
- `to_tier` — the one stronger tier that may take it, or `None`;
- `human_required` — true whenever no stronger model may take it;
- `auto_resume_allowed` — false whenever the dispatch outcome is unknown;
- an `evidence_digest` binding the ticket to that exact state.

The ladder is `small → large → human`. An `uncertain_dispatch` has no model
rung at all: `to_tier` is `None` regardless of the current tier.

Reopening a stopped loop requires an `AttentionGrant` issued by a manager or a
human. The grant is bound to one run, one exact revision, and the specific stop
reason, and it carries an expiry, so a copied or replayed grant cannot revive a
stop the manager did not actually look at. A grant may promote only to the tier
the escalation ticket named. Clearing an `uncertain_dispatch` additionally
requires `acknowledgesUncertainOutcome`: a human stating they reconciled the
outcome by hand.

A grant reopens the loop. It does not refund it: cumulative turns, tokens,
tool calls, and wall-clock are carried across untouched.

## Revisions, restart, and duplicate prevention

Every loop carries a monotonic `revision`, bumped once per accepted step and
once per applied grant, and never reused.

- `admit_step` takes the revision the caller believes it holds. A mismatch is
  `stale_version` and mutates nothing, so a step replayed after a restart is
  rejected rather than counted twice.
- `commit_loop_state` is a compare-and-swap. It refuses a write carrying an
  older revision, and it refuses a same-revision write that moves the dispatch
  backwards. That second rule is what keeps `Uncertain` absorbing across
  processes: a stale worker holding a pre-crash handle cannot overwrite an
  unknown outcome with `Idle` and quietly earn itself a retry.
- On `OrchStore::open`, `recover_loop_dispatches` converts every `Sending` to
  `Uncertain`, alongside the existing run, finalization, and idempotency
  recovery passes. `Idle`, `Delivered`, and `Failed` are all known outcomes and
  are left untouched. Nothing is ever resent on this path.
- The loop ledger is per-run and is pruned with its run record, so it cannot
  accumulate behind retention.

## Public projection

`project_loop` returns counters, enums, bounded labels, and digests only. There
is no prompt, path, command, model output, workspace identity, or run ID in it,
so it is safe on a read surface that a run record itself is not. A stopped loop
projects as `needs_attention` with its reason, not as activity.

## What this does not do yet

The policy core and the durable ledger are wired into the orchestration store
and its restart recovery. Feeding live turn events into `admit_step` from the
running Build turn path is **not** part of this change; see the residual gaps in
the pull request. Until that lands, the loop ledger records what a caller
submits to it rather than observing the turn by itself.
