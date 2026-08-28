# Embedding GrokPtah run and receipt state: a portability contract

**Scope of this document.** What an external project — ContextDesk first — may
read from a GrokPtah host through the existing `ServiceControlPlane` / MCP
seam, what it may rely on, and what it must not assume. It is a review of the
seam as it actually stands on this base, plus the contract that follows from
what the review found.

| | |
|---|---|
| Base | `67e29bd34dc64049432c715c93c2cef2185c63ea` (`origin/main`) |
| Reviewed | `crates/codegen/grokptah-agent-bridge/src/orchestration/**`, `mcp_control.rs`, and the SDK crate |
| Changed | `crates/codegen/grokptah-agent-sdk/**` only. No bridge file is touched. |
| Contract | 1.1 → **1.2** |

**This review did not build the bridge and makes no claim about it.** The
container has no `dbus-1`, so `libdbus-sys` — reached through `keyring` →
`sync-secret-service` → `dbus-secret-service` — fails its build script before
any GrokPtah code compiles, and the bridge's only CI platform is macOS.
Everything asserted below about the runtime comes from reading its source at
the cited lines, not from executing it. Nothing here was installed to work
around that.

---

## 1. What the host actually does

Six facts, each read directly from the runtime. The contract in §3 is built on
these and on nothing else.

| # | Fact | Evidence |
|---|---|---|
| F1 | The authenticated principal is `AuthContext { token_id, owner_id }`. `owner_id` exists and is documented as the hook a later multi-tenant service maps credentials onto "without changing the protocol shape". | `orchestration/authz.rs:8` |
| F2 | A run's `client_id` is **host-authored** from `auth.token_id`, never caller-supplied. The compatibility credential `"primary"` is stamped as the legacy wire value `"mcp"`; every other credential is stamped by its own id. | `orchestration/service.rs:6812` |
| F3 | **Reads do not use the principal.** `authorize_run_request` takes `(session_id, workspace, run_id)` and no `AuthContext` at all; it checks run→session membership and workspace-allowlist match. 57 service methods take `_auth` — underscore-prefixed, deliberately unused — against 34 that use it. Every one of the `*_scoped` reads is in the first group. | `service.rs:5530`, `service.rs:5089`, `grep -c '_auth: &AuthContext'` |
| F4 | A durable receipt is `IdempotencyReceipt { request_id, scope, payload_hash, run_id, tool, response, error, created_at, status }`. `response` is the full replayed body and `error` is a full `OrchError` including its message. `scope` binds the receipt to the principal that claimed the key. | `orchestration/types.rs` |
| F5 | `payload_hash` is **unkeyed** `SHA-256` of the serialized request. For `submit_task` that request contains the prompt. | `orchestration/types.rs:1373` |
| F6 | Retention is not a simple window. Only `complete`/`failed` receipts are expirable; a receipt whose run is **non-terminal** is kept regardless of age or count; an unreconcilable receipt is kept. The 1,000-receipt budget is applied to a **global** `created_at`-descending ordering, not per run. | `orchestration/store.rs:113`, `store.rs:4666` |

Two names in the brief for this review do not exist at this base. `grep` over
every `.rs`, `.ts`, `.tsx`, and `.md` file returns **zero** files for
`RunEngine`/`run_engine`, `SendState`/`send_state`, and
`OperationReceipt`/`operation_receipt`. The nearest real thing is the workload
supervisor, which opens by saying what it is not: *"a recovery loop, not a
second execution engine"* (`orchestration/supervisor.rs:3`). §5 says how both
land additively when they arrive; this document does not model an interface it
cannot read.

---

## 2. What the review found

### F-1 — Scope is session + workspace, never principal (open, host-side)

This is the single most important thing a consumer must understand, and the
contract cannot paper over it.

The host knows which credential created each run (F2) and discards that
knowledge on every read (F3). So two credentials that can reach the same
session see **the same runs and the same receipts**, including each other's. A
seam that told ContextDesk it was reading "its own" runs would be lying.

The contract therefore names the scope it actually has — *this run, in this
session, in this workspace* — and never implies ownership. That scope is still
unforgeable from the consumer side: the workspace must already be in the host
allowlist, the session must own the run, and the SDK's `WorkspaceRef` cannot
name a workspace the host has not reported. It is a real fence; it is just not
a per-principal one.

Closing it is a host change: give `authorize_run_request` the `AuthContext` it
is already handed and filter on the `client_id` the host already stamps, with
the `"primary"`→`"mcp"` mapping applied. That is a behavioral change to the
authorization fence, which this lane does not own. Until it lands, §3's
`ReceiptScope` documents the tier honestly, and a consumer that needs
principal isolation must run a host per principal.

### F-2 — Forward compatibility was documented but not implemented (fixed here)

The crate promised that a minor bump is additive and that unknown values decode
rather than fail. It did not do that. **All sixteen wire vocabularies were
closed enums**, and because they sit inside larger records, one unknown token
failed the whole `RunView` or the whole event page:

```
"unknown variant `provider_operation`, expected one of `create_session`,
 `submit_task`, `follow_up`, `cancel`, `acquire_lease`, `release_lease`, `other`"
```

Two doc comments asserted the opposite of the code — `OperationClass` claimed an
unrecognized tool "projects to `Other`", and `PublicEventKind` claimed an
unknown event decodes to `Unrecognized` "rather than failing the whole page".
Both were false. Any word the host added to any of those vocabularies would
have broken every deployed consumer at once.

Fixed: `src/vocab.rs` defines `open_vocabulary!`, every vocabulary carries an
`Unknown(Label)` arm that round-trips the host's token verbatim, and
`PublicEvent` checks the `kind` against `KNOWN_EVENT_KINDS` before decoding, so
an unknown kind costs one event instead of the page. A *known* kind with a
malformed field still fails — tolerance is for vocabulary, not corruption.

### F-3 — The payload digest was a prompt-confirmation oracle (fixed here)

`ReceiptView.payload_digest` carried the host's `payload_hash` (F5) and was
documented as revealing nothing. It revealed a great deal: guess the prompt,
hash it, compare. The crate already reasons about exactly this weakness for
`WorkspaceRef` — where the guessable secret is a *path*. A prompt is both more
valuable and the one thing this boundary exists to withhold, and by F-1 the
oracle worked across credentials sharing a session, not merely on one's own
traffic.

Fixed: `AttemptDigest::derive(scope_salt, host_payload_hash)` —
`SHA-256(salt ‖ 0x00 ‖ hash)` truncated to 16 bytes. Within one salt this is a
bijection on the host's hash, so *same attempt / different attempt* survives
exactly; without the salt no guess can be tested, and two scopes never agree on
the same payload.

### F-4 — Retention was under-described (fixed here)

`ReceiptRetention { max_receipts, max_age_days }` invited a consumer to compute
"older than 7 days, therefore gone" and be wrong for every receipt attached to
a live run (F6), and to read 1,000 as a per-run allowance when it is a host-wide
budget a noisy neighbour can consume.

Fixed: the window now carries `budget_scope` and `exemptions`, and
`RUNTIME_DEFAULT` states the runtime's real policy.

---

## 3. The contract

### 3.1 Scope

Every read is addressed by `RunSelector { session_id, workspace, run_id }`, and
every one of the three is checked. The scope tier is:

```
ReceiptScope := (session_id, workspace_ref)   // enforced by the host today
             ⊅ principal                       // NOT enforced; see F-1
```

`WorkspaceRef` is an adapter-issued handle, never a path, and resolves only for
a workspace the host has already reported. An unreported ref fails
`workspace_mismatch` **without a round trip**, matching the runtime's
session-independent allowlist gate.

A consumer must not present run or receipt data as belonging to it. "Runs in
this workspace" is true; "your runs" is not.

### 3.2 Reads are idempotent, and what that means under a moving window

A read never mutates. The property that needs stating is the harder one:
**re-reading the same cursor never silently returns a different answer.** It
returns the same items, or it returns `cursor_expired` carrying the retained
range. It never skips a gap quietly — the runtime already holds this line for
events, answering a below-window cursor with HTTP 410 and the window riding the
error (`mcp_control.rs`, `ptah_get_events` / `ptah_get_computer_run_events`),
and receipts inherit it.

Consequences a consumer must encode:

* **Absence is never proof.** A receipt that aged out and a receipt that never
  existed are indistinguishable. This is why `ReceiptPage` carries its
  `ReceiptRetention` inline rather than beside it — you cannot hold the items
  without also holding the caveat.
* **`max_receipts` is not your allowance.** Under `RetentionBudgetScope::Host`
  another run's traffic can expire yours.
* **Two classes never age out**: unsettled receipts, and receipts of
  non-terminal runs. A 9-day-old receipt on a live run is correct, not a bug.

### 3.3 Pagination and cursors

Opaque, ordered, resumable. Receipts sort by `(recorded_at, request_id)` and
the cursor is exactly that composite, so a paged walk yields the unpaged order
and a tie inside one millisecond neither drops nor duplicates. Do arithmetic on
a cursor and you are wrong: it is a position, not an index. A cursor the adapter
did not issue is `invalid_request`, never a silent restart.

### 3.4 Redaction — absence by type

Nothing below is filtered at the boundary; it is **absent from the type**, so a
buggy adapter has nothing to leak.

| Withheld | Why |
|---|---|
| `response` | The replayed body carries whatever the mutation returned — prompts, paths, queue entries (F4). |
| `error.message` | Runtime messages embed absolute paths verbatim; `canonical_workspace` formats one straight into a `workspace_mismatch`. The typed code carries the meaning. |
| `tool` | Host vocabulary. `OperationClass` is the stable classification. |
| `payload_hash` | A prompt oracle (F-3). `AttemptDigest` crosses instead. |
| `workspace` | Absolute path. `WorkspaceRef` crosses instead. |
| `client_id` | Principal attribution the host does not enforce on reads (F-1); publishing it would invite a consumer to build isolation on a value nothing checks. |

### 3.5 Error taxonomy

`SdkErrorCode` mirrors `OrchErrorCode` with byte-identical tokens plus
`stale_observation` / `uncertain_outcome` and four seam-local codes;
`origin()` separates runtime from seam from unrecognized. Unknown codes decode
to `Unknown(String)`.

`retry_disposition()` is **three-valued** on purpose. `uncertain_outcome` is
`Unsafe`, not merely "do not retry": it is the one case where an automatic
retry can double-apply real work. A receipt whose status this build cannot read
is treated the same way — `is_uncertain()` returns `true` for any status that
is not `Complete` or `Failed`, because the dangerous reading is "settled".

### 3.6 Version negotiation

`negotiate()` fails closed on a major mismatch in either direction and reports
the effective minor plus `degraded`. From **1.2** a host may add words to any
vocabulary knowing older consumers degrade to `Unknown`; before 1.2 that
promise was not true (F-2).

---

## 4. Deterministic tests

`tests/portability.rs` — 11 tests, no clock, no randomness, no I/O. Each models
a host one version ahead and asserts the consumer degrades in a defined
direction:

* every one of the sixteen vocabularies decodes an unknown token *and
  re-serializes it verbatim* — a consumer that reads and forwards a record must
  not quietly rewrite it;
* a declared-known token still decodes to its variant (the open arm must not
  swallow the vocabulary it protects);
* an unrecognized token is bounded and control-stripped, so a hostile host
  cannot push escapes into a consumer's log through the tolerance path;
* one unrecognized event costs one event, not the page;
* a *known* kind with a broken field still fails;
* a nested unknown token does not fail the event carrying it;
* a real `RunView`, re-serialized with three words this build lacks, decodes
  whole — identity, revision and timing intact;
* fail-closed predicates: unknown lifecycle is not terminal, unknown status is
  uncertain, unknown digest algorithm refuses to verify, unknown medium is
  `application/octet-stream` rather than a guess.

In `src/dto.rs`: the digest is never the host hash, agrees within a salt,
differs across salts, and retention exempts what the runtime exempts.

In the battery (now 26 checks; 26/0/0 against the fake, 18 passed and 8 skipped
against the scripted service transport):
`observe.vocabulary_is_within_this_build` reports — as `Skipped`, not `Failed`,
because a host is allowed to be ahead — which fields of a live host this build
cannot read. It is the first check to read after a host upgrade.

## 5. Where the two absent systems land

Neither exists at this base (§1), so this is a shape, not an integration.

* **Provider operation receipts.** A provider-side receipt is a *receipt*: it
  has a request identity, a class, a settle state, and a time. It reaches this
  contract as `OperationClass::Unknown("provider_operation")` on an older
  consumer and a named variant on a newer one — a minor bump under 1.2, and
  `tests/portability.rs` already pins that exact token. What must **not**
  happen is a provider name, route, or credential riding along; those are
  `CapabilityId::ProviderCredentials`, permanently forbidden and stamped as
  such by `CapabilityDocument::new` so no adapter can advertise them.
* **A headless run engine.** If one produces runs, it produces `RunRecord`s and
  the existing projection covers it. If it introduces a lifecycle state, that
  state decodes to `RunLifecycle::Unknown`, is treated as non-terminal, and is
  reported by the battery check above. If it introduces a *second* lifecycle,
  that is a major bump and a design conversation, not a decode problem — the
  contract deliberately mirrors one state machine and adds none.

## 6. Smallest macOS-CI change set

**Zero, for everything in this change.** `.github/workflows/desktop.yml`
already runs, on `macos-latest`, in `crates/codegen/grokptah-agent-sdk`:

```
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --no-default-features
```

`crates/codegen/grokptah-agent-sdk/**` is in both the `push` and
`pull_request` path filters, and the crate's `Cargo.lock` is already in the
sccache key. `tests/portability.rs` is picked up by `cargo test --locked` with
no workflow edit.

For the host-side work in §7, the change set is also zero: the `Bridge fmt +
clippy + tests` step already runs `cargo test --locked -- --test-threads=1` in
the bridge, so new bridge tests are covered where they land.

## 7. Residuals and owner

| | Item | Owner |
|---|---|---|
| **P1** | **F-1: principal-scoped reads.** Pass the `AuthContext` that `authorize_run_request` is already handed and filter on the stamped `client_id`, `"primary"`→`"mcp"` applied. Until then no consumer may claim isolation. | **Bridge / orchestration owner.** Touches the authorization fence; not this lane's to change. |
| **P1** | **Host-side `ptah_list_receipts`.** `OrchStore` has `load_idempotency(request_id)` and no listing read at all (`grep -c 'fn list_idempotency'` → 0). Full packet in `AGENT_SDK_SEAM.md`. | **Bridge owner**, on a machine that builds the bridge. |
| **P1** | **Live two-host battery run.** Wire shape is proven against a scripted transport; runtime behavior is not proven against anything. | SDK lane, once a host is reachable. |
| **P2** | Host-issued `WorkspaceRef`s, so opacity does not rest on an adapter-side key. | Bridge owner. |
| **P2** | The five bridge-side findings in `AGENT_SDK_SEAM.md` §*Bridge-side findings*. | Bridge owner. |
| **P2** | Publication decision — `publish = false` until ADR-002 §7 step 5 has a named compatibility owner. | ADR-002 owner. |

The follow-up that unblocks the most is **F-1**, and it is a bridge change, not
an SDK one. Everything this lane could safely fix on this head is fixed.
