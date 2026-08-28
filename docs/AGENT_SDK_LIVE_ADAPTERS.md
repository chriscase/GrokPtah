# Live adapters: the SDK contract against real hosts

**What changed.** PR #431 proved the public seam against a *scripted* transport.
That established the wire shape and nothing about the runtime — no line of the
host had ever answered one of these calls. This branch runs the same versioned
conformance battery against a real `grokptah-service` process and against the
control server the Desktop embeds, and closes what first contact exposed.

| | |
|---|---|
| Base | `67e29bd34dc64049432c715c93c2cef2185c63ea` (`origin/main`) |
| Donor | `19b84a64b3222c36c0db19d2b50b286b3f1454bc` (PR #431 head, untouched) |
| Contract | 1.2, now **declared by the host** rather than assumed |

---

## 1. The matrices

Both adapters run the identical battery through the identical published
`ServiceControlPlane`. There is no Desktop-specific adapter, no second DTO set,
and no bespoke JSON-RPC client — that equality *is* the deliverable.

| Adapter | Result |
|---|---|
| `FakeControlPlane` (deterministic) | 26 passed, 0 failed, 0 skipped |
| Scripted `ptah_*` transport | 18 passed, 0 failed, 8 skipped |
| **Real `grokptah-service` process** | **15 passed, 0 failed, 11 skipped** |
| **Desktop embedded control server** | **15 passed, 0 failed, 11 skipped** |

The two live matrices agree check for check.

**What the Desktop row is and is not.** It drives
`start_control_from_env(host)` — the single call
`desktop/src-tauri/src/lib.rs` makes to start its control plane — with the same
environment contract the Desktop uses. That is the Desktop's *control-plane
entry point*, and it is the **shared control server**, not the packaged Tauri
shell. Nothing here exercises the Tauri application, its IPC, its webview, or a
browser. Read the row as "the Desktop's server answers this contract
identically", never as "the Desktop app is qualified".

### Why each live check is skipped

Every skip is a stated limit, never a silent pass.

| Skipped | Why | Whose gap |
|---|---|---|
| `authz.foreign_workspace_is_workspace_mismatch` | A `WorkspaceRef` exists only once the host has reported that workspace, so there is no ref for a non-allowlisted one to hand back. The property is enforced one layer earlier, by construction. | Not a gap |
| `authz.cross_tenant_read_is_indistinguishable` | The harness runs one owner. Needs a second credential with a distinct `agent_owner_id`. | Harness |
| `faults.lost_connection_is_safely_retryable` | The harness cannot drop an established connection mid-call. | Harness |
| `faults.uncertain_send_is_never_auto_retried` | No wire state produces an uncertain outcome on demand. | Harness |
| `followup.stale_fence_is_rejected_without_effect` | `ptah_steer` has no compare-and-set. | **Host — residual R2** |
| `events.expired_cursor_reports_retained_range` | The harness cannot evict retained events. | Harness |
| `artifacts.*` (3) | The synthetic completion produces no artifacts; real ones need a provider turn. | Harness |
| `lease.*` (2) | No claimable work item is seeded. | Harness |

## 2. What running against a real host found

Divergences found by running against a real host, plus what review found
in the code behind it. None was reachable from a script written to the
contract.

### F-0 — Reads were not bound to a principal (fixed, and proven)

The host stamps `client_id` from the authenticated credential when a run is
created, then discarded that knowledge on every read: `authorize_run_request`
took no `AuthContext` at all, and 57 service methods took `_auth`. Any
credential that could reach a session could read **every run in it**, including
runs another credential created.

`authorize_run_request` now consumes `auth` and binds the principal, using the
same `stamped_client_id` function that writes the value — one function, so the
write and the check cannot drift. A run with no `client_id` predates
attribution and is refused rather than shared: fail-closed is the only safe
reading, since the alternative grants everyone access to exactly the runs whose
owner is unknown.

Proven, not asserted:
`a_second_principal_cannot_read_the_first_principals_run` runs a real service
with two named device credentials. Principal A submits a run and reads it back.
Principal B — a different credential, the same session, the same allowlisted
workspace, the workspace reference learned legitimately from the host — is
refused, and its refusal is byte-identical in **code and message** to the
refusal for a run that never existed. Receipts obey the same fence.

That second half is the point: a principal check that answered "exists but not
yours" differently from "no such run" would close the read and open an oracle
for probing other principals' run ids.

**Host authority is named, never inferred.** Managed execution authors runs
under one explicit identity — `native-executor` — and those belong to the
account rather than to whichever device triggered the manager tick. That single
id is matched exactly, as `HOST_AUTHORED_CLIENT_ID`.

An intermediate version of this fence inferred host authority instead: it
treated any `client_id` *absent from the current credential set* as
host-authored. That was wrong in a way worth recording, because it looked
reasonable. Removing or rotating credential A drops A from the set, so every
run A had ever created would have been reclassified as host-authored — and
become readable by A's replacement. Legacy and unrecognized ids shared the same
fate. Authority derived from an absence is authority that appears whenever
configuration changes.

The decision table is now exactly: the caller's own stamped id, allowed; the
named host identity, allowed; **everything else refused** — no attribution at
all, an unrecognized or legacy id, a rotated-away credential, the desktop
client, and any other configured device.

`rotating_a_credential_does_not_share_its_history` proves it across a real
two-process restart: A creates a run, the service is stopped and restarted
against the same durable home with A removed from its credentials, and B is
refused with the identical code *and message* it gets for a run that never
existed.

**No public read accepts an `AuthContext` and ignores it.** Six entrypoints —
`get_run`, `get_progress`, `get_events`, `get_changes`, `get_test_results`,
`get_handoff` — took `_auth` and called `load_authorized_run` directly. MCP
dispatch happened to use the scoped variants, so nothing was exploiting it, but
the public surface was bypassable by any present or future caller. They now go
through `load_principal_bound_run`, which binds the principal even where no
session or workspace is supplied.

`load_authorized_run` now has exactly two callers, both binding wrappers, and
`no_read_reaches_a_run_without_binding_a_principal` is a source-level guard
that keeps it that way — the failure it prevents is structural, and a future
read that bypassed the fence would compile and pass every behavioural test that
did not happen to cover it.

**What this does not do.** It binds the principal and the current credential
set — `AuthContext` is derived per request, so a revoked credential fails at
`auth_header`. It does **not** implement capability-generation revalidation
(#458), a trusted broker, app authentication, CSRF, or opaque session binding.
Bearer possession plus `with_operator_authority` remains operator-equivalent,
and no browser or cross-product safety is claimed. `publish = false` stands.

### F-6 — The idempotency key namespace was global (fixed, and proven)

Reported in review, confirmed in code, and the most serious of the three: the
receipt namespace had no principal in it at all. `idemp_path(request_id)`
resolved to `<root>/idempotency/<sha256(request_id)>.json` and
`IdempotencyReceipt` carried no owner field. Hashing the id keeps it off the
filesystem; it does not make the namespace anyone's. Every caller on a host
shared one, for a value **the caller chooses**.

Three consequences, all reachable by a second credential simply picking a key:

* **Cross-principal read.** Same key, same payload hash → `Replay`, handing the
  second caller the first caller's stored `response` verbatim.
* **Existence oracle.** Same key, different payload → `conflict`, which
  confirms the key is taken. That is exactly the oracle F-1 removed from run
  reads, on a different surface.
* **Squatting.** A caller could claim a key before its owner used it.

Identity is now `(scope, request_id)`. The scope is derived from the same
`stamped_client_id` the run fence compares — one derivation, so run ownership
and receipt ownership cannot drift — and is stored both in the file name (as a
fixed-width hex prefix, so a caller-chosen id cannot forge one) and inside the
receipt, which `claim` and `finish` both check. A foreign key now looks
*unused*: `Perform`, not `Replay` and not `conflict`.

**The wire value is unchanged.** Callers still send, and receipts still report,
exactly the `request_id` they chose; `ptah_*` argument and result schemas are
untouched. Only where a receipt is stored, and which receipts a lookup can
reach, are scoped — which is what #466 requires of the provider-attempt key.

Host-authored work (the native executor, managed intents, in-process resume)
claims in one named scope, `IdempotencyScope::host()`, built from the same
`HOST_AUTHORED_CLIENT_ID` the run fence names. There is now a single definition
of that constant.

A receipt written before scoping existed carries no scope. It cannot be
attributed to a principal, so it is served to nobody and the existing retention
policy drains it — the same fail-closed posture as a run with no `client_id`.
The one visible consequence, stated rather than hidden: a retry that spans the
upgrade is treated as a new request rather than replayed.

Proven at both levels. `receipts_are_scoped_per_principal` and
`a_legacy_unscoped_receipt_is_unreachable` pin the store behaviour, including
that the *owner's* replay still works — scoping must not cost the owner the
guarantee the key exists for. `one_principals_idempotency_key_does_not_reach_anothers`
drives it through the real service over HTTP with two credentials: B's create
under A's key succeeds with a different session, and A's retry still replays A's.

### F-8 — Nothing exercised a mutation that landed and lost its answer (fixed)

Review's third point, and a fair one: the earlier evidence covered a restart
and covered idempotent creation, but never the case that makes durable receipts
worth having — the request reaches the host, **takes effect**, and the response
never comes back. Two live checks skip precisely there
(`faults.lost_connection_is_safely_retryable`,
`faults.uncertain_send_is_never_auto_retried`) because the harness cannot arm
the fault, and "skipped" was being read as "covered elsewhere". It was not.

A fake cannot produce this honestly: dropping a call *before* it lands is a
different failure with a different correct answer. So the fault is now injected
around a real transport — `PostEffectDisconnect` makes the call, waits for the
host to act, and only then discards the response.

Two tests, deliberately separated, because they prove different things.

`a_lost_response_is_reconciled_after_a_host_restart_in_process` isolates the
host lifecycle: the caller creates a session under a key and never sees the
answer; the effect is shown durable *before* anything restarts, so a later
failure cannot be blamed on the write never happening; the host stops and
reopens against the same home; the retry is handed the session it already has.
**This is not a process restart** — an earlier version of this document called
it one, which it is not: the allocator, the tokio runtime, every `static` and
the advisory instance lock all survive a stop-and-reopen in one test process.

`a_killed_service_process_replays_its_receipt_to_a_second_process` is the
process-boundary proof, and the one that answers the review. It spawns the
`grokptah-service` **binary** as a child OS process over a real socket, takes
the mutation there, discards the response before the caller sees it, then
**SIGKILLs** the child — no unwinding, no `Drop`, no flush, so whatever is on
disk is whatever was already committed. A second child starts against the same
home; the test asserts the two PIDs differ, that the effect survived, and that
retrying the key returns the existing session. It also proves in passing that
the advisory instance lock is released by process death rather than by a clean
shutdown path.

Both assert the session count at every step, so both failure directions — a
lost effect and a duplicated one — fail. Verified as guards, not assumed:
pointing the second child at a *different* home fails the durability assertion
(`left: 0, right: 2`) rather than passing quietly. `ChildService` kills its
child from `Drop`, so a failing assertion cannot leak a process that would hold
a port, a home and the instance lock and make the *next* test fail for an
unrelated reason.

### F-9 — The scope was sealed by convention, not by the type (fixed)

Review's sharpest point, and correct. F-6 scoped the *storage*, but left every
key to that storage public: `IdempotencyScope` was re-exported from the crate
root, `for_client_id` took a `&str`, `HOST_AUTHORED_CLIENT_ID` was a public
constant, and eight `OrchStore` methods were `pub fn` taking a scope. So any
in-process or out-of-tree caller could spell another principal's scope — or the
host's — and read, claim, settle or list its receipts, and mint cursors for
them. The HTTP fence was real; the boundary underneath it was decorative.

Three changes, all structural:

* **A scope cannot be built from a string.** `for_client_id(&str)` is gone. The
  only constructors are `of(&AuthContext)` — which requires the caller's own
  authenticated context — and `host()`, the single named host identity. Naming
  a namespace now requires the authority it stands for.
* **Nothing is re-exported.** `IdempotencyScope` and `HOST_AUTHORED_CLIENT_ID`
  are `pub(crate)` and absent from the crate root, so no caller outside this
  crate can name either.
* **Every receipt method is `pub(crate)`.** `save`, `load`, `claim`, `complete`,
  `fail`, `list_idempotency_for_run`, `receipt_cursor` and
  `parse_receipt_cursor`. `IdempotencyReceipt` itself went with them: it carries
  the full replayed `response` and an `error` with its message, which is exactly
  what `PublicReceipt` exists to project away from, and exporting a type whose
  every producer is crate-private only advertises an affordance that is not
  there.

`no_public_function_accepts_a_scope` keeps it that way as a source-level guard —
verified to fail when one method is made `pub` again. A behavioural test would
not notice: a `pub fn` handed a scope compiles and passes everything else in the
file.

The sharp remaining case is stated rather than hidden:
`the_host_namespace_is_not_reachable_by_spelling_its_name` asserts that a
credential *named* `native-executor` does land in the host scope. That is a
deployment constraint on credential naming, not something this type can close.

### F-10 — Settlement rewrote the page ordering key (fixed)

`finish_idempotency` stamped `created_at = Utc::now()`, so the key a listing
orders by was mutable. The failure is precise: a caller takes page one and holds
a cursor at the receipt it just saw; that receipt is still pending; it settles;
its key jumps *past* the cursor; page two hands the caller the same receipt
again.

The claim time is now immutable for the receipt's whole life, and settlement is
recorded separately as `settledAt`. Retention still ranks by settlement (falling
back to the claim for a receipt that never settled), because ageing a settled
receipt from a moment before its work began would be wrong in the other
direction.

`settling_a_receipt_does_not_move_it_past_an_issued_cursor` is written in the
failing shape — the receipt that settles mid-walk is the one page one already
returned — and was confirmed to fail against the original code
(`["beta", "alpha"]`) before the fix.

### F-7 — The page cursor was a claim, not a check (fixed)

The contract said "a cursor this host did not issue is `invalid_request`,
never a silent restart". The host checked that the string *looked* like
`millis:request_id` and nothing else, so the sentence was false in both
directions: a caller could seek to a position never handed out, and a value
documented as opaque was plainly readable and constructible.

Cursors are now authenticated with the same per-home secret the attempt digest
uses (`HMAC-SHA256`, verified here against the RFC 4231 vectors rather than
trusted), and bound to the scope and run they were issued for. So a cursor
cannot be forged, cannot be replayed onto another run or by another principal,
and carries no readable structure — hex throughout, tag first at fixed width,
so nothing in the payload can be mistaken for a delimiter. Every rejection
reason returns the same refusal, so *why* a cursor failed is not an oracle
either.

The SDK's in-process fake keeps its own simpler encoding, and its
documentation now says so instead of describing it as opaque: a consumer that
learned to build a cursor from the fake would break against every real host.

`a_receipt_cursor_is_authenticated_and_bound` pins the store behaviour, and
`a_forged_receipt_cursor_is_refused_by_the_live_host` drives the real service
over HTTP with the exact shapes the old parser accepted.

### F-4 — The SDK dropped the idempotency key it was handed (fixed)

`create_session` built its arguments from the workspace and title only, so the
caller's `request_id` never reached the wire — even after the host learned to
deduplicate. Meanwhile `transport_unavailable` classifies as safely retryable,
so a consumer following that advice after a disconnect would create a second
session.

Both halves are fixed. The key is transmitted when the host advertises support
(checked, not assumed: an older host declares `additionalProperties: false` and
would reject it). Where it cannot be sent, a dropped or ambiguous create is
re-coded `uncertain_outcome` — the case the three-valued retry disposition
exists for — with the original code preserved as a detail for diagnosis.
`creating_a_session_twice_under_one_key_yields_one_session` proves the live
path: two calls, one session, and the session count rises by exactly one.

### F-5 — Receipt ordering and its cursor disagreed on precision (fixed)

Ordering compared the full `created_at`; the cursor carried only milliseconds.
Two receipts inside one millisecond whose sub-millisecond order is the inverse
of their request-id order straddled the page boundary in opposite directions,
and the second was skipped on resume. Both now use the same truncated key, and
`same_millisecond_receipts_with_inverse_ids_are_never_skipped` builds exactly
that pair and walks it one page at a time.

### F-1 — An existence oracle on every run read (fixed)

An unknown run answered `invalid_request`; a run in another session answered
`forbidden_scope`. A caller could therefore **probe run ids for existence** —
and since reads here are scoped by session and workspace rather than by
principal, the ids being probed need not be the caller's own.

All six denial sites now return one indistinguishable refusal through a single
`run_not_available()` helper. A *malformed* id keeps `invalid_request`: that is
a format error about the caller's own input and discloses nothing about what
exists.

### F-2 — "Cancel is idempotent" was an assumption from the fake (fixed)

The live runtime refuses to cancel an already-terminal run, which is
defensible. The check assumed the first call succeeds. Idempotence is
*agreement between the two calls*: the check now passes when both succeed alike
or both refuse alike, and fails when a first call mutates and the second
disagrees.

### F-3 — Version negotiation was vacuous (fixed)

`CapabilityDocument` stamped the **consumer's** `CONTRACT_VERSION` as if it were
the host's, so `negotiate()` compared this build against itself and could never
disagree. Hosts now declare their own contract via `ptah_get_host_info`, and
the document carries what the host said. A negotiation that cannot fail is not
a negotiation.

## 3. Host-side closures

Each is exercised by the live battery, not asserted in prose.

* **`ptah_list_receipts`** — durable receipts for one run, behind the same
  `authorize_run_request` fence as every other scoped read, ordered
  `(created_at, request_id)` with a matching composite cursor, bounded 1–200.
  A cursor the host did not issue is `invalid_request`, never a silent restart
  — authenticated and bound to the scope and run it was issued for (F-7), so
  that is a check rather than a claim about shape.
* **Retention travels with the page.** The window reports the runtime's real
  policy — a **host-wide** budget of 1,000 that also exempts unsettled receipts
  and receipts of non-terminal runs. A consumer reading `maxReceipts` as a
  per-run allowance, or concluding an old receipt must be gone, would be wrong
  both times.
* **Host-issued salted attempt digests.** The stored `payload_hash` is an
  unkeyed `SHA-256` of the request, and for `submit_task` that request contains
  the prompt. Publishing it would hand every bearer holder a
  prompt-confirmation oracle. The host salts with a per-home secret
  (`<root>/receipt-digest.key`, 0600, created on first use) so the raw hash
  never leaves the host, and `AttemptDigest::from_host` validates what arrives
  rather than trusting it.
* **Idempotent session creation, scoped to the caller.** `ptah_create_session`
  takes an optional `request_id`. Absent keeps the previous behavior exactly;
  present makes the one mutation with no request identity replayable. Because
  the key is a value the *caller* chooses, receipt identity and lookup are
  scoped to the authenticated principal (F-6): reuse is a conflict for its
  owner and simply unused for anyone else.
* **`maxTotalTokens` is advertised.** `merge_bounds` always accepted it, but the
  schema omitted it under `additionalProperties: false`, so a schema-validating
  client was refused the one documented ceiling it most needed.
* **`ptah_get_host_info`** — product, host version, contract major/minor.
  Deliberately thin: no build paths, no feature flags, no topology. A version
  endpoint is not a reconnaissance surface.

## 4. The reference consumer

`crates/codegen/grokptah-sdk-reference-consumer` is written the way ContextDesk
would be, and is interesting for what it **cannot** do. Its whole dependency
graph is 32 crates and contains no `grokptah-agent-bridge`, no
`grokptah-service`, no `keyring`, `reqwest`, `axum`, or `tauri` — asserted
against its own lockfile, so the proof cannot rot into a comment.

It also pins, as tests: a filesystem path cannot decode into a `WorkspaceRef`;
a lease credential reaches neither JSON nor `Debug`; Computer Use control and
provider credentials are permanently forbidden regardless of what a host
advertises; an unknown capability counts as a mutation; a lifecycle this build
cannot read is still watched rather than assumed finished; and an uncertain
outcome is never advertised as retryable.

## 5. Residuals

**R0 — The auth story beyond principal binding.** Capability-generation
revalidation (#458), a trusted broker with app authentication, CSRF and
revocation, opaque session binding, and separately gated Computer Use
authority. Bearer possession plus operator authority remains
operator-equivalent, and **no browser or cross-product safety is claimed**.

**R1 — Monotonic run revisions.** `Revision` still derives from `updatedAt`
milliseconds, so two commits inside one millisecond collapse. Closing it means
adding a counter to the durable `RunRecord`, bumping it on every save, and
exposing it on `ptah_get_run`. **Not done here.**

**R2 — Compare-and-set steering.** `ptah_steer` has no CAS, so a fenced
follow-up is refused rather than fenced, and
`followup.stale_fence_is_rejected_without_effect` skips against both live
hosts. Depends on R1: a fence needs an authority that a millisecond timestamp
cannot provide. **Not done here.**

**R3 — Host-issued `WorkspaceRef`.** Refs are still derived adapter-side from
`SHA-256(key ‖ path)`. With the default key that obfuscates a low-entropy path
without hiding it. The receipt digest now shows the right shape — a per-home
secret, issued by the host — and refs should follow it. **Not done here.**

**R4 — Rust/JSON/TypeScript schema parity from one source.** The Rust types are
the only generator today; a TypeScript consumer still hand-mirrors them.
**Not done here.**

**R5 — Principal binding: done here; the rest of the auth story is not.**
Run, event, artifact and receipt reads are now principal-bound and proven
against two live credentials (F-0), and idempotency keys are scoped to the
principal that chose them (F-6). What remains, and what no consumer may
assume: capability-generation revalidation (#458), a trusted broker with app
authentication, CSRF and revocation, opaque session binding, and separately
gated Computer Use authority. Bearer plus operator authority is still
operator-equivalent. These belong to #455/#460/#458/#461/#462 and the safe
assembly order; this branch does not pretend to them.

**R6 — Bounded session pagination.** `list_sessions` fetches the host's whole
session list and pages locally. The bound belongs at the host.

**R7 — Reference consumer is compile-shape only.** It proves the dependency
graph and the type-level containment, not an authenticated transport, a
restart, or a broker. It is not yet the "second real consumer" ADR-002 §7
step 4 requires.

**R8 — Legacy idempotency receipts are drained, not migrated.** Receipts
written before F-6 carry no scope, so they are served to nobody and age out
under the existing retention policy. A retry that spans the upgrade is treated
as a new request rather than replayed. This is deliberate — the alternative is
to guess an owner for a record that has none — but it is a one-time behaviour
change and a host operator should know it happens rather than discover it.

**R10 — The scope is derived from the stamped credential id, and that is
provisional.** It is *not* canonical authority: there is no owner identity, no
auth generation, and no delegation chain in it, so a renamed credential is a
different principal and a reused name inherits the old namespace without an
epoch to separate them. That binding belongs to #460/#458. What is done here is
to make the eventual change a migration rather than a silent orphaning: every
receipt records `SCOPE_DERIVATION_VERSION` alongside its scope, so a canonical
rule arrives as version 2 with a readable before and after. The digest was also
widened from 64 to 128 bits — enough is not the same as ample for a value that
separates principals. `scope_follows_the_credential_and_records_how_it_was_derived`
pins both consequences so neither is a surprise. **No consumer may read the
current scope as canonical identity.**

**R9 — Two live fault checks still skip.**
`faults.lost_connection_is_safely_retryable` and
`faults.uncertain_send_is_never_auto_retried` remain skipped in both matrices:
the conformance harness has no way to arm a fault, and F-8 injects one around
the transport in a dedicated test rather than through the battery. Teaching the
`Harness` trait to arm faults would let the battery cover this on every host
instead of one. **Not done here.**

## 6. Publication

`publish = false` stands. Before that can change, ADR-002 §7 step 5 needs:

1. **A named compatibility owner** accountable for the version matrix.
2. **A support commitment** — which contract majors are maintained and for how
   long, and what a consumer is owed on a breaking change.
3. **An upgrade path** — how a consumer pinned to 1.x learns 2.0 exists, and
   what the deprecation window is.
4. **A second real consumer.** One consumer cannot distinguish a contract from
   an interface shaped around a single caller.

R5 is a hard gate on top of those: an SDK published while any bearer can read
any run in a shared session would be publishing a weaker boundary than its
documentation implies.

## 7. Verification

Bridge builds on Linux here only because `libdbus-1-dev` was installed into the
container (`keyring`'s `sync-secret-service` feature pulls `libdbus-sys`).
CI still runs macOS only, where the native keychain backend applies, so
**Linux-green is not hosted-green** and the hosted `desktop` job remains the
authority.

| Check | Result |
|---|---|
| SDK `fmt` / `clippy -D warnings` / `test --locked` | clean; 125 tests |
| SDK feature matrix (default / none / fake / conformance) | clean |
| Reference consumer `fmt` / `clippy` / `test` | clean; 8 tests |
| Bridge strict clippy, the exact CI command | clean apart from the two macOS-gated `computer_use` findings present at base |
| Bridge `cargo test --locked --no-fail-fast -- --test-threads=1` | 720 passed; the 2 pre-existing failures below |
| `sdk_live_conformance` (2 battery drivers + 9 focused) | 11 passed |
| Live service + Desktop battery matrices | 15 passed / 0 failed / 11 skipped each, agreeing |

The bridge sweep runs with `--no-fail-fast`. Without it `cargo test` stops at
the first failing target and never reaches the ones after it — which is how an
earlier packet reported a clean local sweep and then went red in CI.

The nine focused live tests: two-principal run reads, redacted receipts, host
contract version, session idempotency, credential rotation across a host
restart, cross-principal idempotency keys (F-6), forged page cursors (F-7), a
post-effect disconnect reconciled across a host restart, and the same fault
reconciled across a **SIGKILLed child process** (F-8).

The failures are **established pre-existing** rather than assumed. Each was
re-run with the bridge source reverted to the base — whose tree (`3a801b5e`) is
byte-identical to `origin/main` — and failed identically there:

* `mcp_continuity_probe::continuity_probe_is_evidence_first_and_recoverable`,
  a 90-second subprocess timeout (98.5s to fail, at base and at head alike).
* `service_smoke::service_mcp_contract_covers_scoped_live_reconnect_controls_and_restart`,
  where `ptah_cancel` returns `invalid_request` because the run has already
  reached a terminal state under `GROKPTAH_AGENT_OFFLINE=1`. The hosted live
  step therefore runs `--test sdk_live_conformance` only, rather than adopting
  another lane's red.
* `mcp_soak_hardening::soak_desktop_bootstrap_node_campaign` fails
  intermittently on a known advisory-lock race and passes on re-run. It is
  reported here because a failure that is only sometimes there is exactly the
  kind that gets quietly dropped from a summary.

All are Linux-container artifacts; the hosted `desktop` job runs macOS, where
the native keychain backend applies and none reproduces.

### Service-crate clippy

`cargo clippy --all-targets -D warnings` on `grokptah-service` reports two
dead-code findings in `tests/common/mod.rs` (`other_workspace`,
`other_workspace_path`). That module is compiled into each of the crate's three
test targets and those items are used by the other two, so the finding is an
artifact of per-target dead-code analysis and is present at base. Nothing in
this branch touches `tests/common/mod.rs`, and CI runs `cargo test --locked
--test sdk_live_conformance` for this crate rather than clippy.

### Bridge clippy

`cargo clippy --locked --all-targets -- -D warnings` reports two dead-code
errors in `computer_use/macos_observation.rs`. That code is macOS-gated, so it
is genuinely unreachable on Linux and live on the CI platform. They appear in a
bare `cargo check` of the unmodified base, and nothing in this branch touches
`computer_use/`. With `-A dead_code` the bridge is clippy-clean.
