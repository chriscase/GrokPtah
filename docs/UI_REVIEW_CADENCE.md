# Recurring expert UI/UX review cadence

Status: **operational process only; no expert review is claimed by this
document.** A review closes a cadence gate only when its secret-free evidence
record validates against the exact assembled integration head and the external
review store contains the referenced visual evidence.

## When a review is required

Create a review record at the first applicable trigger:

| Cadence | Trigger | Release effect |
| --- | --- | --- |
| `integration_wave` | Each material operator-surface integration wave, or every two significant GUI changes, whichever comes first | P0/P1 findings block the next integration wave; lower findings need a disposition and tracking reference. |
| `periodic` | The agreed team interval, and at least once per active development cycle | A missing or stale review blocks the next packaged acceptance checkpoint. |
| `release` | Every packaged release candidate, after the assembled app is built | Any unresolved P0/P1 or incomplete accessibility/state matrix blocks release. |

The review is against the exact assembled integration SHA, not a mockup,
prototype, `tauri:dev` window, stale screenshot, or a different worktree.
Record the SHA before opening the packaged app and refuse to continue if the
working tree or package identity drifts.

## Reviewer and evidence rules

The reviewer must be an independent skilled UI/UX reviewer (human or clearly
identified review model/tool) who did not author the last surface wave. The
record must include:

- opaque reviewer and tool IDs, review time, cadence, and exact candidate SHA;
- packaged-window surfaces and operator workflows exercised;
- wide/narrow and light/dark states;
- empty/loading/success/error/denied/permission/exhausted/reconnecting states;
- hostile long text and overflow behavior;
- keyboard coverage, focus order/visibility, screen-reader labels/status,
  contrast, zoom/reflow, and reduced-motion behavior; and
- severity-ranked findings with opaque evidence digests and tracking refs.

Screenshots, recordings, prompts, source excerpts, private reviewer notes,
credentials, and local paths remain in the external evidence store. The
checked-in record contains only the secret-free projection defined by
[`UI_REVIEW_EVIDENCE.md`](UI_REVIEW_EVIDENCE.md).

## Review procedure

1. Freeze the assembled SHA and package identity; record the exact window
   surfaces and workflow list.
2. Run the complete state and accessibility matrix in the packaged build.
3. Record each finding once, assigning P0–P3 severity and a disposition.
4. Fix P0/P1 findings before the next integration/release gate, or stop with
   an explicit failed review. P2/P3 findings require a tracking reference or
   an evidence-backed accepted tradeoff.
5. Run the evidence validator and retain the secret-free record plus external
   evidence digests. Never set `claim_eligible` by hand.
6. Link the record from the integration PR/release record and schedule the
   next cadence review immediately.

## Ownership and retention

The release owner is responsible for the trigger calendar, exact-SHA check,
reviewer independence, and unresolved-finding block. The UI owner is
responsible for fixes and regression coverage. The operations owner retains
the external evidence store and ensures its digests remain resolvable without
exposing screenshots or private notes.

This cadence supplements the one-time packaged acceptance required by roadmap
stage 10; it cannot be replaced by a single polish pass or by declaring core
UX gaps unsupported.
