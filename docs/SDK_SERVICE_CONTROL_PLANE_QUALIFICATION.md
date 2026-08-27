# SDK ↔ service control-plane qualification

**Gate.** Repository `chriscase/GrokPtah`, source branch
`codex/external-worker-hardening-v1`, base and head
`8ad3be07eb27087acb67704fdf463ecb95b64505`. Work branch
`claude/sdk-service-adapter-qualification-8n5t72`. Toolchain `1.92.0`
(`rust-toolchain.toml`), Linux x86_64, no network reachable from any test.

**Verdict.** The named artifact — an SDK `ServiceControlPlane` adapter, and a
`grokptah-service` transport to qualify it against — **does not exist at this
SHA**. Neither name appears anywhere in the working tree or in any commit
reachable from any ref (`git log --all -S`). `crates/codegen/grokptah-service/`
is listed in `README.md`'s repository layout table but has never been created.
Live qualification of that artifact is therefore **not claimed**.

What this change does instead is qualify the seam that actually exists, against
the transport that actually exists, and record precisely where the SDK contract
and the live service disagree.

---

## 1. What was qualified, and why it is not a scripted double

The service under test is the production loopback MCP control plane
(`crates/codegen/grokptah-agent-bridge/src/mcp_control.rs`), started through
`start_control_server`, which is the same entry `start_control_from_env` uses
for the desktop host and the live coordinator smoke harness.

Every assertion in `tests/sdk_service_control_plane.rs` crosses:

| Layer | Real? | Notes |
| --- | --- | --- |
| TCP socket | yes | `TcpListener` bound to an ephemeral `127.0.0.1` port |
| HTTP + JSON-RPC framing | yes | `reqwest` client; MCP Streamable HTTP re-derived in the harness, **not** the in-tree `McpControlClient`, so a client-side assumption cannot mask a wire change |
| axum router / `/mcp` route | yes | `POST`/`GET`/`DELETE` as registered in production |
| bearer auth middleware | yes | `authenticate_request`, constant-time compare |
| transport session table | yes | server-issued `mcp-session-id`, `initialized` flag |
| tool allowlist | yes | `CONTROL_TOOLS` / `FORBIDDEN_TOOLS` |
| orchestration policy | yes | `OrchestrationService`, real `OrchStore` on disk |
| idempotency ledger | yes | durable claim/replay/conflict records |
| Computer Use policy, leases, audit, redaction | yes | real `ComputerUseService` + `ComputerStore` |
| public error projection | yes | `grokptah_agent_sdk::ErrorEnvelope` built by `json_err` |

Two things are harness-owned, and both are stated rather than hidden:

1. **The host-side backend owner.** `ComputerRunController` is a trait with no
   implementation inside `grokptah-agent-bridge`; the desktop registers
   `DesktopComputerUse` (`desktop/src-tauri/src/computer_use.rs:776`), and that
   crate is a separate nested Cargo workspace this crate's tests cannot depend
   on. The harness installs `HarnessComputerController`, which mirrors the
   desktop's MCP mutation path one-for-one: the same owner-session-plus-workspace
   scope filter as `controller_run`, the same `grant_request.validate(run.limits)`,
   the same delegation into the shared `ComputerUseService`, and the same
   server-derived `client.actor_id()` as grant actor. It adds no policy, so
   every lease, revision fence, audit entry, and redaction under test is the
   real service's behaviour.
2. **The observation backend.** `SimulatorBackend`, the deterministic backend
   already used by the in-tree Computer Use suites. No real display, guest, or
   host input is involved; this lane is disjoint from the packaged isolated-guest
   work.

The environment is hermetic: a disposable `GROKPTAH_HOME` (`tempfile::TempDir`
plus `set_grokptah_home_override`), a synthetic workspace, and
`GROKPTAH_AGENT_OFFLINE=1`. No provider is contacted, no credential is read, no
user data is reachable. The only secret in play is a loopback bearer token
minted per test.

**Why a double would not have found the headline result.** The central finding
below is that the SDK's *request* DTOs serialize to a shape the live routes
reject. A scripted double echoes whatever the test author sent, so it would
have reported success. The live route rejects it because its argument structs
are snake_case with `deny_unknown_fields`. That asymmetry is only observable
against the real transport.

---

## 2. Conformance matrix (SDK DTO ↔ live MCP route)

Measured over the live transport, not read off the source.

| Direction | SDK type | Live route | Wire-compatible as-is? |
| --- | --- | --- | --- |
| request | `SubmitTaskRequest` | `ptah_submit_task` | **No** — camelCase vs snake_case, `deny_unknown_fields` → `invalid_request` |
| request | `ComputerControlRequest` | `ptah_authorize_computer_run` | **No** — nested `scope` vs flattened args, camelCase → `invalid_request` |
| request | `RunScope` | every scoped read | **No** — `sessionId`/`workspace`/`runId` vs `session_id`/`workspace`/`run_id` |
| response | `DurableRun` | `ptah_get_run` | **Yes** — `RunRecord` is a superset; extra fields are ignored |
| response | `RunEventPage` | `ptah_get_events` | **Yes** — `JournalPage` matches `entries`/`nextCursor`/`cursorExpired`, entries match `seq`/`ts`/`update` |
| response | `CapabilitySet` | `initialize` → `serverInfo.capabilityContract` | **Yes** — built from the SDK type by `capability_contract::advertised_capabilities` |
| response | `ErrorEnvelope` | JSON-RPC `error.data`, all routes | **Yes** — built from the SDK type by `json_err` |
| response | (none) | `ptah_submit_task` receipt | **N/A** — the accept receipt (`runId`/`sessionId`/`state`/`requestId`/`executionMode`/`queuedPosition`) is not `DurableRun`; the SDK has no type for it |
| response | `ComputerControlResponse` | `ptah_authorize_computer_run` | **No** — the route returns `ComputerRunProjection`, which has `version` and `controlDisposition` but no `scope`; the adapter must synthesize the response from the caller's own fence |
| response | `ComputerEventPage` | `ptah_get_computer_run_events` | **No** — entries are `ComputerAuditEntry` (`sequence`/`at`/`operation`/…), not `ComputerEvent` (`seq`/`ts`/`kind`/`detail`); the adapter must map field by field |

Shared enum vocabularies **do** agree on the wire and are asserted as such:
`DurableRunState` ↔ `RunState`, `ExecutionMode` ↔ `RunExecutionMode`,
`ComputerActionClass` ⊂ `ActionClass`.

The translation these rows require is exactly the missing adapter. It is
implemented in `tests/sdk_control_plane_harness/mod.rs` as
`SdkServiceControlPlane`, deliberately by hand, so a wire change breaks a test
rather than being absorbed silently.

---

## 3. Behaviour qualified over the live transport

`cargo test --test sdk_service_control_plane` — 10 tests, green, five
consecutive runs, ~1.3s.

| Requirement | Test | Result |
| --- | --- | --- |
| `initialize`, protocol negotiation, session issuance | `initialize_and_tools_list_expose_exactly_the_live_allowlist` | dated protocol negotiated; `mcp-session-id` issued; advertised contract is `grokptah.capabilities.v1`; `run.promote` stays `human_gate: true` |
| `tools/list` matches the allowlist exactly | same | set-equal to `CONTROL_TOOLS`; no `FORBIDDEN_TOOLS` member discoverable |
| read-only by default | `undiscoverable_and_forbidden_tools_are_denied_as_public_envelopes` | `ptah_create_session`, `ptah_delete_session`, `run_terminal_cmd`, `ptah_manage_mcp`, `ptah_set_config`, and an unknown name all → `forbidden_scope` / HTTP 403; denial never echoes the probed name; read routes on the same session stay reachable |
| session creation is host authority | same | not reachable from the transport at all; sessions are minted host-side |
| SDK requests need explicit translation | `sdk_request_dtos_require_explicit_translation_at_the_live_routes` | untranslated `SubmitTaskRequest` and `ComputerControlRequest` → `invalid_request`; the adapter's translation is accepted by the same route |
| task submission, `get_run`, events | `durable_run_projections_and_event_pages_satisfy_the_sdk_contract` | receipt → durable terminal state; `DurableRun` deserializes and validates; `RunEventPage` deserializes, sequences strictly increase, `next_cursor` names the last entry, resume does not replay |
| cancellation | `cancelling_a_live_run_reaches_durable_cancelled_over_the_transport` | a live run reaches durable `cancelled`; the side effect never lands; replaying the cancel `request_id` returns the identical receipt |
| exact (session, workspace, run) binding | `run_reads_require_the_exact_session_workspace_and_run_triple` | owner reads; bystander session → `forbidden_scope`; un-allowlisted workspace claim → `forbidden_scope`; no privileged detail in either denial |
| request-id replay / conflict | `request_id_replay_is_idempotent_and_a_changed_payload_conflicts` | identical replay returns the same `runId` (no forked run); same key with a changed payload → `invalid_request` / `reasonCode: conflict`; exactly one run exists for the key |
| manager-issued grants are the only mutation authority | `computer_mutations_require_an_initialized_client_and_mint_server_side_grants` | an authenticated but uninitialized client cannot mutate (`forbidden_scope`) while its reads still work, and the durable revision does not move; an initialized client's grant records `GrantIssuer::McpClient` whose id is `"<clientName>@<clientVersion>#<server-issued session id>"`; a caller-supplied `client_id` argument is rejected, so the actor is not forgeable |
| lease boundaries | `computer_control_is_revision_fenced_and_projects_observation_staleness` | `ttl_ms: 0` rejected; `uses_remaining` honoured; `expires_at > issued_at`; requested action classes preserved exactly |
| revision fencing | same | a stale `expected_version` is refused and the durable revision does not advance; a spent revision is not reusable |
| stale vs fresh observations | same | a just-captured observation projects `stale: false` under the default 10s bound; a run created with `max_observation_age_millis: 200` projects `stale: true` after 700ms — a real fresh→stale transition read through the live route |
| redacted public projections | same, plus `computer_audit_pages_are_redacted_and_map_onto_the_sdk_event_page` | observation summaries carry no screenshot and no element tree; audit pages carry no host path, home, or token; error envelopes carry only the public taxonomy |
| Computer Use events and cancellation | `computer_audit_pages_are_redacted_and_map_onto_the_sdk_event_page` | audit sequences strictly increase, the grant appears as `authorize`, cursor resume does not replay and is not reported expired, and a live cancel leaves a terminal run |

---

## 4. Findings

### P1-A — `grokptah-agent-sdk` does not compile at the gate SHA

`cargo build` of the crate fails under the pinned toolchain:

```
error: missing documentation for a struct field
   --> crates/common/grokptah-agent-sdk/src/run.rs:230:13
230 |     Event { scope: RunScope, event: RunEvent },
```

`#![deny(missing_docs)]` now covers enum struct-variant fields, and
`RunNotification::Event`'s two fields are undocumented while the sibling
`Recovery` variant's are documented. Because `grokptah-agent-bridge` depends on
the SDK by path, **nothing downstream builds either** — the bridge's entire test
suite was unreachable at this SHA.

*Fixed here* by documenting the two fields. Doc comments only: no API, serde, or
behaviour change.

### P1-B — the SDK's own test target does not compile at the gate SHA

```
error[E0382]: borrow of moved value: `recovered`
   --> crates/common/grokptah-agent-sdk/src/error.rs:86:9
```

`serde_json::to_value(recovered)` moves the value, which is then read on the
next assertion. *Fixed here* by borrowing. Test-only.

### P1-C — an SDK unit test asserted an error message the code never returns

`external_worker::tests::launch_rejects_host_paths_and_control_identities`
expected `"worker identity contains a control character"` for a `starting_ref`
carrying `\n`. `starting_ref` is routed through `validate_ref`, which returns
`"worker ref must be bounded and non-absolute"`. The rejection itself is
correct and fail-closed; only the expected string was wrong.

*Fixed here* by asserting the shipped message and adding a case that reaches
`validate_identity` through `model`, so the test still covers what its name
claims. **Owner decision required:** if the intended contract is a distinct
control-character message for refs, that is a one-line change in
`validate_ref` instead, and this test should then be re-tightened.

Taken together, A–C establish that the SDK crate was never built or tested at
`8ad3be07` under the repo-pinned toolchain.

### P1-D — the public error taxonomy loses retriability on the fenced surfaces

`computer_mutation_error` folds `ComputerErrorCode::Conflict` **and**
`StaleObservation` into `OrchErrorCode::Conflict`, which `public_error_code`
maps to `ErrorCode::InvalidRequest`. Observed live:

```json
{"code":"invalid_request","message":"The request conflicts with current state.",
 "reasonCode":"conflict","requestId":null}
```

for a stale `expected_version`. `OrchErrorCode::StaleVersion` already maps to
`ErrorCode::StaleOrRecovery`, but these routes never emit it. The SDK documents
`StaleOrRecovery` as "Cursor, revision, or approval is stale" — precisely this
case — so an SDK consumer cannot separate a retriable stale revision from a
malformed request on the exact surface where revision fencing matters most.
Only the non-normative `reasonCode` carries the distinction.

The fence itself holds: the mutation is refused and the durable revision does
not advance. **Not fixed** — the mapping is production policy and sits outside
this lane's allowlist. Suggested one-line change: map
`Conflict | StaleObservation` to `OrchErrorCode::StaleVersion` in
`computer_mutation_error`.

Pinned by `computer_control_is_revision_fenced_and_projects_observation_staleness`.

### P1-E — `CursorCloudAdapter` rejects every provider response (casing bug)

The gate commit ("feat: harden external worker boundaries") made
`worker_record` require that a Cursor response *prove* both safety properties:

```rust
if agent.auto_create_pr != Some(false) || agent.work_on_current_branch != Some(false) {
    return Err(InvalidResponse(
        "Cursor response did not prove PR creation and current-branch writes are disabled"))
}
```

`CursorAgent` is `#[serde(rename_all = "camelCase")]`, so `auto_create_pr`
deserializes from **`autoCreatePr`** (lowercase `r`). Cursor's field — and the
value this same adapter *sends* on launch (`src/external_worker.rs:336`) — is
**`autoCreatePR`**. The key never binds, `#[serde(default)]` leaves it `None`,
and the check trips unconditionally. Verified in isolation against the crate's
own fixture body:

```
auto_create_pr = None   work_on_current_branch = Some(false)
hardened check trips = true
```

`work_on_current_branch` is unaffected: camelCase derives `workOnCurrentBranch`,
which matches.

Consequence: the adapter refuses **every** Cursor response, correct ones
included, so the Cursor Cloud path is non-functional. The failure direction is
safe — nothing unsafe is accepted — but there is no working lifecycle at this
SHA. The crate's only adapter lifecycle test,
`external_worker::tests::fake_cursor_api_covers_launch_poll_artifacts_and_terminal_cancel`,
fails for exactly this reason (`src/external_worker.rs:1035`), which contradicts
`docs/ROADMAP_TO_100.md`'s claim that "its fake API fixture covers the core
lifecycle without credentials."

**Not fixed** — production code outside this lane's allowlist, and the owner
should confirm Cursor's actual response field name before choosing the fix.
Suggested one-liner: `#[serde(rename = "autoCreatePR")]` on the field.

### P2-A — durable-run reads leak run existence

`load_authorized_run` returns `invalid_request` / `"unknown run_id"` for a
missing record but `forbidden_scope` for a record owned by another session. A
caller holding a valid session+workspace fence can therefore enumerate which
run ids exist. The Computer Use read path deliberately collapses both cases
into one denial — `authorize_computer_scope` documents that "session existence
is not distinguishable from cross-scope" — so the two surfaces disagree on a
property one of them treats as a security boundary.

**Not fixed** — changing it is a production behaviour change outside this
lane's allowlist. Pinned, with the divergence named, by
`run_reads_require_the_exact_session_workspace_and_run_triple`.

### P2-B — three in-tree suites assert the pre-redaction error contract

With the compile fixes applied, `cargo test --test mcp_streamable_transport`
gives 22 passed / 3 failed, and `cargo test --lib` gives 315 passed / 2 failed.
Four of those five share one root cause: the public `ErrorEnvelope` redaction
landed, but these consumers still assert the internal codes and messages. (The
fifth, the Cursor fixture, is P1-E above.)

| Failure | Expected | Live |
| --- | --- | --- |
| `unknown_and_forbidden_tools_fail_closed_over_http` (:886) | message contains `"not available"` | `"The requested scope is not allowed."` (`reasonCode: forbidden_scope`) |
| `request_timeout_returns_error` (:845) | message contains `"timeout"`, `data.code == "timeout"` | `"The request exceeded its bounded deadline."`, `data.code == "internal"`, `reasonCode: timeout` |
| `live_computer_reads_node_smoke` (:2325) | Node conformance expects `code: "cursor_expired"` (56/57 checks pass) | `code: "stale_or_recovery"`, `reasonCode: "cursor_expired"` |
| `mcp_control::tests::computer_reads_fail_closed_when_the_ledger_is_unavailable` (`src/mcp_control.rs:3064`, unit test) | `data.code == "unsupported"` | `"invalid_request"` (`OrchErrorCode::Unsupported` → `ErrorCode::InvalidRequest`) |

The shipped redaction looks correct in each case; the assertions are stale.
**Not fixed** — these are existing suites outside this lane's allowlist, and
the second one also exposes a taxonomy question the owner should settle:
`OrchErrorCode::Timeout` maps to `ErrorCode::Internal`, so a bounded deadline
is publicly indistinguishable from an unexpected failure, which again destroys
retriability. That is the same defect as P1-D on a different route.

### P2-C — no `grokptah-service` crate, and no in-crate `ComputerRunController`

`README.md` documents `crates/codegen/grokptah-service/` as the "standalone
local/VM/private-cloud service host"; it does not exist. Separately,
`ComputerRunController` has no implementation reachable from
`grokptah-agent-bridge`, so the crate's MCP mutation routes cannot be exercised
end-to-end without a host-installed adapter. Both facts bound what any
qualification in this crate can claim.

### P1-F — every committed lockfile is stale, and it fails the `desktop` CI check on every PR

When the bridge gained `grokptah-agent-sdk` as a path dependency, **none** of
the three committed lockfiles were regenerated. All three lack a
`grokptah-agent-sdk` entry, so every `--locked` command fails:

```
error: the lock file .../Cargo.lock needs to be updated but --locked was passed
```

| Lockfile | Missing entry | Breaks |
| --- | --- | --- |
| `Cargo.lock` (root) | `grokptah-agent-sdk` | `cargo … --locked` in the root workspace |
| `crates/codegen/grokptah-agent-bridge/Cargo.lock` | `grokptah-agent-sdk`, and it in the bridge's own dep list | the workflow's `Bridge fmt + clippy + tests` step |
| `desktop/src-tauri/Cargo.lock` | same | the workflow's `Cargo tests (desktop)` step — **first**, so nothing after it ever runs |

This is not theoretical. The `desktop` check on
[PR #441](https://github.com/chriscase/GrokPtah/pull/441) fails at
`Cargo tests (desktop)` after 48 seconds
([run 33057720113](https://github.com/chriscase/GrokPtah/actions/runs/33057720113/job/98468564247)),
before it reaches any bridge step. `.github/workflows/desktop.yml` triggers on
`desktop/**`, `crates/codegen/grokptah-agent-bridge/**`, and `evals/**`, so
**any** PR touching those paths hits it.

Proof it is pre-existing and not caused by this branch: the commit here touches
no file under `desktop/` and no `Cargo.toml`, and `cargo metadata --locked` in
`desktop/src-tauri` fails identically on this tree, whose `desktop/` directory
is byte-identical to the base SHA. Unlocked resolution succeeds and produces a
**purely additive** 9-line diff — one package stanza plus one dependency line,
no version changes:

```
+ "grokptah-agent-sdk",
...
+[[package]]
+name = "grokptah-agent-sdk"
+version = "0.1.0"
+dependencies = [ "serde", "serde_json" ]
```

**Not committed here.** All three are shared build inputs — `desktop/src-tauri/Cargo.lock`
is a packaged-build input this lane was told to preserve, and lockfiles conflict
readily across concurrent branches. This is a repo-wide blocker that wants one
owning lane, not a drive-by fix on a qualification branch. The fix is to run
`cargo metadata` (or any unlocked cargo command) once in each of the three
workspaces and commit the additive result.

### P3 — pre-existing lint and format drift (not touched)

* `cargo fmt --check` reports drift in
  `crates/codegen/grokptah-agent-bridge/src/external_worker.rs` (1.92 rustfmt
  formats `assert!` chains differently). Only the new files were formatted;
  reformatting that file is outside the allowlist.
* `cargo clippy -p grokptah-agent-sdk` reports one `collapsible_if` at
  `external_worker.rs:196`, pre-existing and outside the changed lines.

---

## 5. What is **not** qualified

* No SDK `ServiceControlPlane` type was created. The adapter lives in the test
  harness, where it is the object under test; promoting it into the SDK would
  contradict that crate's stated contract ("no Tauri, provider, filesystem,
  network, credential, or execution policy dependency") and is an owner
  decision, not a qualification step.
* No `grokptah-service` transport exists, so nothing about a standalone service
  host is claimed.
* No provider, gateway, credential, or external-worker path was exercised.
  `CursorCloudAdapter` remains unqualified here.
* No real display, guest, packaged helper, or host input. This lane is disjoint
  from the packaged isolated-guest and Computer Use benchmark lanes.
* Cursor-expiry (`cursorExpired: true`, `eventRange`) on durable runs is not
  exercised: forcing journal rollover needs a flood this suite deliberately
  does not run. The type is covered by the SDK's unit tests and by the existing
  Node conformance script.
* `ptah_act` / semantic action dispatch is not reachable from the control plane
  and is not covered.

## 6. Command results

All from `crates/codegen/grokptah-agent-bridge` unless noted, toolchain 1.92.0.

| Command | Result |
| --- | --- |
| `cargo test --test sdk_service_control_plane` | **10 passed / 0 failed** (5 consecutive runs, no flake) |
| `cargo test -p grokptah-agent-sdk` (root workspace) | **12 passed / 0 failed** + 0 doc-tests |
| `cargo test --test orchestration_control` | 37 passed / 0 failed |
| `cargo test --test orchestration_adversarial` | 18 passed / 0 failed |
| `cargo test --test mcp_computer_mutations` | 2 passed / 0 failed |
| `cargo test --test bridge_lifecycle` | 41 passed / 0 failed |
| `cargo test --test computer_use_release_gate` | 6 passed / 0 failed |
| `cargo test --test isolation_capability` | 5 passed / 0 failed |
| `cargo test --lib` | 315 passed / **2 failed** — both pre-existing (P1-E, P2-B) |
| `cargo test --test mcp_streamable_transport` | 22 passed / **3 failed** — all pre-existing (P2-B) |
| `cargo clippy --test sdk_service_control_plane` | 0 warnings in `tests/`; 8 pre-existing in `src/` |
| `cargo clippy -p grokptah-agent-sdk --all-targets` | 1 pre-existing `collapsible_if`, outside changed lines |
| `cargo fmt --check` | new files clean; pre-existing drift in `src/external_worker.rs` left untouched |
| `cargo metadata --format-version 1` | valid for both the bridge crate and the root workspace |
| `cargo build --locked` | **fails in all three workspaces** — pre-existing stale lockfiles (P1-F) |

The five failures are **not** regressions from this change: at the gate SHA the
SDK does not compile (P1-A), so the bridge's test suite could not be built or
run at all. They are the state observed at gate HEAD **plus** the minimal
doc-and-test-only compile fixes in P1-A/B/C, none of which alter runtime
behaviour.

## 7. Reproducing

```sh
cd crates/codegen/grokptah-agent-bridge
cargo test --test sdk_service_control_plane          # 10 passed
cargo test -p grokptah-agent-sdk --manifest-path ../../../Cargo.toml   # 12 passed
```

`libdbus-1-dev` and `pkg-config` must be installed for the `keyring`
dependency to build on Linux.
