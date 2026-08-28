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

Two divergences, neither reachable from a script written to the contract.

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

**Host-authored runs are account-scoped, not device-scoped.** Managed
execution authors runs under synthetic identities — the native executor stamps
`native-executor` — and those belong to the account, not to whichever device
triggered the manager tick. `principal_may_read` therefore allows a run whose
`client_id` is not one of *this service's configured device credentials*, and
refuses one that is but does not match. The set is read from configuration
rather than a hardcoded name, and both sides are compared in stamped form so
the `primary` → `mcp` mapping applies consistently. Refusing host-authored runs
would have broken managed execution without closing any cross-device hole,
because no device created them.

**What this does not do.** It binds the principal and the current credential
set — `AuthContext` is derived per request, so a revoked credential fails at
`auth_header`. It does **not** implement capability-generation revalidation
(#458), a trusted broker, app authentication, CSRF, or opaque session binding.
Bearer possession plus `with_operator_authority` remains operator-equivalent,
and no browser or cross-product safety is claimed. `publish = false` stands.

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
  A cursor the host did not issue is `invalid_request`, never a silent restart.
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
* **Idempotent session creation.** `ptah_create_session` takes an optional
  `request_id`. Absent keeps the previous behavior exactly; present makes the
  one mutation with no request identity replayable, with key reuse a conflict.
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
against two live credentials (F-0). What remains, and what no consumer may
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
| SDK `fmt` / `clippy -D warnings` / `test --locked` | clean; 124 tests |
| SDK feature matrix (default / none / fake / conformance) | clean |
| Reference consumer `fmt` / `clippy` / `test` | clean; 8 tests |
| Bridge `cargo test --locked -- --test-threads=1` | 533 passed, 1 pre-existing failure |
| Service crate `cargo test --locked -- --test-threads=1` | 19 passed, 1 pre-existing failure |
| Live service + Desktop batteries | 15/0/11 each, agreeing |
| Live focused tests (two-principal, receipts, host version, session idempotency) | 6 passed |

Two failures, both **established pre-existing** rather than assumed. Each was
re-run with the bridge source reverted to the base — whose tree (`3a801b5e`) is
byte-identical to `origin/main` — and failed identically there:

* `mcp_continuity_probe::continuity_probe_is_evidence_first_and_recoverable`,
  a 90-second subprocess timeout (98.5s to fail, at base and at head alike).
* `service_smoke::service_mcp_contract_covers_scoped_live_reconnect_controls_and_restart`,
  where `ptah_cancel` returns `invalid_request` because the run has already
  reached a terminal state under `GROKPTAH_AGENT_OFFLINE=1`.

Both are Linux-container artifacts; the hosted `desktop` job runs macOS, where
the native keychain backend applies and neither reproduces.

### Bridge clippy

`cargo clippy --locked --all-targets -- -D warnings` reports two dead-code
errors in `computer_use/macos_observation.rs`. That code is macOS-gated, so it
is genuinely unreachable on Linux and live on the CI platform. They appear in a
bare `cargo check` of the unmodified base, and nothing in this branch touches
`computer_use/`. With `-A dead_code` the bridge is clippy-clean.
