# Issue #466: operator reconciliation and scoped attempt views

This slice extends the existing host authority and provider-attempt lattice; it
does not create a second send ledger and it never performs provider I/O.

## Surface

- `list_scoped_attempt_projections` and `scoped_attempt_projection` return
  secret-, URL-, credential-, and path-free views bound to the authenticated
  principal, session, and workspace. Unknown and foreign handles return the
  same empty result.
- `mint_reconciliation_grant` issues a short-lived, one-use
  `OperatorReconcile` lease bound to the attempt, revision, durable state,
  dialect, route digest, principal, and authority generations.
- `apply_reconciliation` supports `Review`, `MarkNotSent`, `MarkSettled`, and
  `Discard`. Review is audit-only; Mark Not Sent requires host-proven absence of
  wire admission; Mark Settled requires a provider receipt or independent
  operator observation digest; Discard is explicit. Every disposition is
  revision-CAS protected, idempotent for the same decision, and never resends.

Restart recovery replays the typed reconciliation audit event before classifying
open attempts. A failed audit append or snapshot write leaves the attempt
ambiguous rather than silently changing its truth.

## Evidence

`cargo test -p xai-host-authority --locked --offline -- --test-threads=1`,
`cargo clippy -p xai-host-authority --locked --offline --all-targets -- -D warnings`,
`cargo fmt --all`, and `git diff --check` are required gates for this candidate.
The focused reconciliation integration suite covers scope collapse, redaction,
pre-wire proof, receipt/observation proof, expiry, revision races, restart
recovery, and crash cuts.

## Nonclaims

This source candidate does not prove live provider behavior, provider receipt
formats, gateway delivery, packaged/native/TCC/VM behavior, or multi-node
coordination. A separate exact-head review and hosted checks are required before
publication or merge, and Issue #466 remains open until those gates are met.
