# Expert UI/UX review evidence

`ui_review_evidence.rs` defines the provider-neutral evidence record for the
recurring expert review cadence in roadmap stage 10. It is a release contract,
not a claim that a review has already happened.

The operational trigger, reviewer-independence, and retention procedure is in
[`UI_REVIEW_CADENCE.md`](UI_REVIEW_CADENCE.md).

## What a review must bind

Every record names:

- the exact assembled Git SHA, reviewer identity, review tool, cadence type,
  and review time;
- real packaged-window evidence for the selected surfaces and workflows;
- a fixed state matrix spanning wide/narrow, light/dark, empty/loading,
  success/error, denied/permission, exhausted/reconnecting, long text, and
  overflow;
- keyboard completeness, focus visibility/order, screen-reader labels/status,
  contrast, zoom/reflow, and reduced-motion evidence; and
- severity-ranked findings with opaque evidence digests and tracking refs.

The record contains no screenshots, prompts, source excerpts, credentials, or
private reviewer notes. Those remain in the external evidence store addressed
by the digests.

## Release rule

`claim_eligible` can be true only for a packaged-window record whose complete
matrix and accessibility checks validate. P0/P1 findings must be fixed or
explicitly accepted; deferred findings require a tracking reference. Unknown
fields, malformed SHAs/digests, duplicate states/findings, missing matrix
coverage, or an incomplete accessibility check fail closed.

During development, create an `integration_wave` record after each material
operator-surface integration wave (or every two or three significant GUI
changes). Create a `periodic` record on the agreed review interval and a
`release` record against the final packaged candidate. A dated record must
refer to the exact candidate SHA; mockups and stale screenshots do not satisfy
the cadence gate.

This contract does not substitute for the required expert review, packaged
visual acceptance, or hardware/Computer Use proof. It makes those outcomes
auditable and prevents a prose-only “looks good” claim from becoming a 100%
release assertion.
