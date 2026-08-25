# Grok Build account status and Run attribution — draft handoff

Status: **draft slice; source/CI proof only.** This document makes no live
provider, qualification, Computer Use, or Stage 5/6 claim. Nothing here was
exercised against a real Grok Build session, a real provider response, or a
signed helper.

This is the first vertical slice of the "Codex-like editor driven by the user's
existing Grok Build session" lane. It adds the account seam the editor was
missing and stops there; it does not introduce a second provider stack, a second
credential resolver, or a second run queue.

## Exact source

- repository: `chriscase/GrokPtah`
- start head (PR #399, verified before editing): `fccb7fc58aa7d0727c4daa344a3d78966fabefbd`
- parent of that head: `bd7a2e11b09d310689f127144d400e2997750c58`
- base branch of #399: `cursor/ci-374-clippy-reexport-guest-tmp-acbc` @ `520d228d79ca7b0428426809cf195ddf493c3623`
- branch for this slice: `claude/grokptah-codex-editor-slice-pzrlmc`
- `origin/main` at the time of the run (**not** used as the source): `67e29bd34dc64049432c715c93c2cef2185c63ea`

The branch previously pointed at `67e29bd`, which is exactly `origin/main` — it
carried no unmerged work, so re-pointing it at the #399 head discarded nothing.

Not merged, not undrafted, not retargeted, not published. No release. No other
GrokPtah, ContextDesk, or NexaForge session or branch was touched.

## The gap this closes

`~/.grok/auth.json` is already read by `auth_store::load_grok_build_session`,
and the record it parses already carries `expires_at`, `principal_id`,
`user_id`, `team_id`, and the account address. All of it was then **discarded**:
the only thing reaching the editor was

```rust
pub struct AuthState { signed_in: bool, display_name: Option<String>, method: Option<String> }
```

Three consequences, all of which this slice addresses:

1. **An expired Grok Build session reported `signed_in: true`.**
   `load_grok_build_session` deliberately falls back to `best_expired` when every
   scope is expired, and `load_auth_state` maps any resolved credential to
   `signed_in: true`. The editor had no field that could say otherwise, so the
   first evidence of an expired session was a mid-run `401` with no account
   context.
2. **`method` was a free-form, file-controlled string.** It is built as
   `format!("grok_build:{mode}")` from `auth_mode` inside `auth.json`. A closed
   vocabulary already existed — `provider_observation::CredentialMethod` — but
   was wired only into the diagnostics recorder, never into the editor.
3. **The public Run contract had no account attribution at all.**
   `PublicProviderRouteSummary` reports provider, model, effort, and quota, but
   nothing about *which credential class* the run's tokens were spent on.

## What changed

### New contract module — `grok_account`

`crates/codegen/grokptah-agent-bridge/src/grok_account.rs` is the reusable,
headless- and browser-safe half. It is a pure function of
(`GrokAccountFacts`, `now`): no I/O, no environment reads, no async runtime, so
ContextDesk or a headless lab can import it and reproduce the exact bytes the
desktop renders.

The no-secret guarantee is **structural, not a runtime filter**:
`GrokAccountFacts` has no field a bearer, refresh token, or API key can land in,
so `project_grok_account_status` cannot serialize credential material even if a
later edit forgets to check.

- `GrokCredentialMethod` — closed vocabulary whose wire names are pinned equal to
  `provider_observation::CredentialMethod` by test, so the recorder and the
  editor cannot drift apart.
- `GrokSessionState` — `active | expiring | expired | no_expiry | unknown | absent`.
- `usable` — the run gate. False **only** on positive evidence
  (`expired`, `absent`). An OIDC session whose `expires_at` this build cannot
  parse reports `unknown` and stays usable: it still authenticates, and blocking
  it would break working installs. Unproven is reported as unproven, not as
  `active`.
- `account_ref` — one-way handle over durable principal fields only. Deliberately
  **narrower** than `WireCredentials::qualification_identity_fingerprint`, which
  falls back to digesting the bearer or refresh token when no principal exists
  and is therefore an oracle for anyone holding a candidate credential. This one
  has no such fallback: it requires `principal_id`, `user_id`, or `team_id`, and
  otherwise returns `None`. Stable across access-token rotation.
- `account_label` — masked address (`a…@example.com`). The human name in a
  display string is dropped, not masked.

### Run attribution — `credentialMethod`

`PublicProviderRouteSummary` gains one field, derived at projection time from the
**shape** of the durable `credential_ref` already frozen on the route.

`credentialRef` and `credentialFingerprint` stay on
`FORBIDDEN_PUBLIC_RUN_KEYS` and are **not** re-exposed under a new name — both
are derived from credential material, and re-publishing either (or any
deterministic function of one) would let a holder of a candidate key confirm it.
Only the class crosses the wire, so a `keychain:` profile name never escapes.
`credentialMethod` is added to `PUBLIC_PROVIDER_ROUTE_KEYS`; the exact-allowlist
and forbidden-key fences are unchanged and still enforced.

No durable schema version changed. No store migration. Receipts written before
this field decode as `Unknown` via `#[serde(default)]` — fail-closed, never
reported as a Grok Build run.

### Editor payload

`AuthState` gains `account: Option<PublicGrokAccountStatus>`, `#[serde(default,
skip_serializing_if)]`. `signed_in` keeps its existing meaning so older desktop
clients are unaffected; `account.usable` is the field a run gate should read.
The existing `auth_state` Tauri command carries it with no command-surface
change. `sign_in_local` — a display-only session with no credential — reports
`absent`, so the gate is shut rather than open.

## Changed-file allowlist (exact)

| File | Change |
| --- | --- |
| `crates/codegen/grokptah-agent-bridge/src/grok_account.rs` | **new** — contract module + 18 unit tests |
| `crates/codegen/grokptah-agent-bridge/tests/grok_account_contract.rs` | **new** — 10 contract tests via public exports only |
| `crates/codegen/grokptah-agent-bridge/src/lib.rs` | module declaration + re-exports |
| `crates/codegen/grokptah-agent-bridge/src/auth_store.rs` | `account_method`, `account_facts`, projection in `load_auth_state` / `store_api_key` |
| `crates/codegen/grokptah-agent-bridge/src/types.rs` | `AuthState.account` |
| `crates/codegen/grokptah-agent-bridge/src/host.rs` | two `AuthState` literals |
| `crates/codegen/grokptah-agent-bridge/src/orchestration/public_run.rs` | `credentialMethod` field, projection, allowlist key |
| `desktop/src/lib/protocol.ts` | TS mirror of both contracts |
| `docs/GROK_BUILD_ACCOUNT_ATTRIBUTION_HANDOFF.md` | **new** — this file |

## Evidence

Linux cloud container, `rustc 1.92.0 (ded5c06cf 2025-12-08)`, `cargo 1.92.0`.

The mandated macOS Stage 6 soak target
`/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default`
**does not exist in this environment** (verified by `ls`), and `ps` showed no
competing `cargo`/`rustc`/soak process before any build started. All local Cargo
used an isolated `CARGO_TARGET_DIR` under this session's scratchpad, serially.
The protected soak target was never a build target here.

`libdbus-1-dev` and `pkg-config` were installed into the container so the
existing `keyring` dependency links on Linux, and `npm ci` was run in
`desktop/`. Both are environment setup; no manifest, lockfile, or dependency was
changed.

| Gate | Result |
| --- | --- |
| `cargo fmt -- --check` | clean |
| `cargo check --lib` at `fccb7fc`, before editing | pass — baseline, 3 pre-existing warnings |
| `cargo check --lib --all-targets` after the slice | pass — same 3 warnings, none new |
| `cargo test --lib grok_account` | **18 passed, 0 failed** |
| `cargo test --test grok_account_contract` | **10 passed, 0 failed** |
| `cargo test --test native_executor_mcp` | 19 passed, 0 failed |
| `cargo test --test orchestration_control` | 42 passed, 0 failed |
| `cargo test --lib` at `fccb7fc` (baseline) | 741 passed, **4 failed** |
| `cargo test --lib` on this branch, run 1 | 759 passed, **4 failed** |
| `cargo test --lib` on this branch, run 2 | 759 passed, **4 failed** |
| `cargo clippy --all-targets -- -D warnings` at `fccb7fc` | **fails** — 3 dead-code errors |
| `cargo clippy --all-targets -- -D warnings` on this branch | **fails identically** — same 3 errors |
| `npm run typecheck` (desktop) | pass |
| `npm run test` (desktop vitest) | 48 files, 379 tests passed |

### Reading the four `cargo test --lib` failures

They are **pre-existing on Linux and not caused by this slice**. The same four
fail at the untouched `fccb7fc` baseline:

```
computer_use::isolated_visual::tests::serialized_contract_contains_no_host_paths_or_channel_secret
computer_use::isolated_visual_channel::tests::canonical_binding_vector_matches_freestanding_guest
computer_use::isolated_visual_runtime::tests::runtime_requires_binding_before_channels_and_stop
computer_use::macos_observation::tests::secure_values_are_removed_and_evidence_is_exactly_scoped
```

741 → 759 passing is exactly +18, this slice's new unit tests, with the failing
set unchanged. None of the four is in a file this slice touches.

### Reading the clippy failure

Also pre-existing on Linux, and identical at baseline: three `dead_code` errors
for macOS-only items (`verified`, `native_context` ×2) in
`computer_use/isolated_visual_artifacts.rs` and
`computer_use/macos_observation.rs`, promoted to errors by `-D warnings`. No
clippy finding names a file this slice touches. **This gate is red on Linux
before and after; it is not evidence for this slice either way.** It must be run
on macOS to be meaningful.

### One observed flake, recorded rather than omitted

The first full `cargo test --lib` run on this branch showed a fifth failure,
`mcp_control::tests::computer_reads_fail_closed_when_the_ledger_is_unavailable`
(got HTTP 200, expected 405). It did **not** reproduce in two subsequent full
runs, and it passes in isolation on this branch. The test depends on
process-global state (`set_grokptah_home_override` plus a `home_override_serial`
guard) and asserts that an exclusive `ComputerStore` lock taken outside the host
blocks the host's ensure; a `200` means the host resolved a different home than
the outside lock did. That is an ordering/parallelism hazard in the test itself.
This slice adds no test that touches the home override. Not fixed here, and not
claimed as unrelated with certainty — recorded so the next operator can watch
for it.

## Remaining unverified gates — not claimed

- No live Grok Build / OIDC session was exercised. Every account projection in
  this slice was driven by synthetic facts and a fixed clock.
- No live provider request, response, usage counter, or quota reservation.
- The desktop UI is **not** wired to gate the Run control on `account.usable`,
  and renders no expiry warning. The contract, the payload, and the TS types
  exist and typecheck; consuming them in `App.tsx` / `SettingsPanel.tsx` is the
  next step and is where this slice becomes visible to a user.
- `desktop/src/lib/protocol.ts` was hand-mirrored. No generator or cross-language
  schema test pins the Rust and TS contracts together; a drift test is a natural
  follow-up.
- Nothing here touches Computer Use capability, status, or approval DTOs. The
  goal's Computer Use preference was assessed as already substantially present
  (`computer_use::{types, projection, policy}`) and was deliberately left alone.
- No macOS build, no Virtualization.framework, no signed helper, no guest boot,
  no hardware matrix, no Stage 5/6 soak evidence, no 100% qualification.
- Hosted CI has not run against this branch.

Leave as draft. No merge, undraft, retarget, or publish.
