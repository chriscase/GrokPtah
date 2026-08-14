# Plan: read-only MCP coordinator surface for Computer Runs (slice 2 of #271)

Status: **design complete, implementation not started** — produced by a
read-only audit session whose container disk (≈37 GiB total writable
allowance, 30 GiB free) was below the mandated 50 GiB build floor. See
[Storage blocker](#storage-blocker). Nothing in this document was compiled,
installed, or executed live; every claim is backed by reading exact code at
the pinned revisions below.

Pinned revisions:

- `main` = `73bd473bc2631098885fe9e99149f63300ee1e66` (post Computer Use stack).
- PR #292 head = `bf4aff2aa83d8f2caee61d94195c9ab5e9db7414`
  (`claude/mcp-computer-run-orchestration-fc3s4o`, draft, base `main`). This
  branch (`claude/mcp-coordinator-live-proof-pr60hg`) is stacked on that exact
  head; #292's branch itself was not modified.
- Issue links: #271 (parent contract), #286 (visible activity / MCP-originated
  activity), #287 (background-safe tier — out of scope), #288 (isolated visual
  backend — out of scope).

## 1. Adversarial review of #292 — result

Verdict: **no new confirmed P0/P1 findings.** Nothing was filed on the PR;
the two structural constraints below were already documented on #271
(2026-08-14 comment) and re-filing them would be duplicates. Properties
verified against code, with evidence:

| Claimed property | Verified against |
|---|---|
| Redaction-safe by construction | `computer_use/projection.rs` types omit element roles/labels/values/ids, geometry, evidence `asset_id`/hash. Transitively safe: `ComputerAuditEntry` (`types.rs:420-428`) carries only enums, ids, and 64-byte-truncated static operation/disposition strings — `error_code` is the enum, never a message; every `ComputerError` message in `service.rs`/`policy.rs`/`simulator.rs`/`macos_native.rs` is static or interpolates only enum/state/version values; `ActionOutcome` summaries are static ("set demo name"); `display_name` is the application name, never a window title (`macos_observation.rs:237`, pinned by test at `:1157-1158`). |
| Non-oracle scoped reads | `load_owned_run` maps id-validation failures and unknown/cross-session lookups to one identical `Unauthorized` error; tests assert equality of all three. Note this is deliberately **stronger** than the pre-existing Build-run reads, where unknown → `invalid_request "unknown run_id"` (`orchestration/service.rs:623`) but cross-session → `forbidden_scope` (`:1092-1096`) are distinguishable. |
| Cursor semantics | `project_events`: limit clamped 1..=500, caller cursor `saturating_add`, `after == start_seq - 1` is continuity, below-window is `cursor_expired` with empty page, final page returns `next_cursor: None`, empty journal yields no false expiry. |
| Restart recovery | `store.rs:262-283` marks non-terminal runs `Interrupted`, bumps `control_epoch`, clears grant + observation, sets a static `Interrupted` last error; #292's test proves the projection reports it and events stay replayable. |
| Disposition precedence | `computerActivity.ts` switches on disposition first and fails closed on unknown dispositions. Misleading combinations are unreachable: `cancel` sets `Stopped` (`service.rs:421`), `Paused → Completed` is not in the transition table (`types.rs:711-742`), and `validate_run_record` (`store.rs:391-399`) rejects inconsistent durable records. |
| GUI/MCP parity | Byte-identical serialization test (desktop-held record vs scoped read); cockpit status renders exclusively from the projection; `run` is retained only for local approval detail. |
| Frontend/a11y | Nine activity cases including fail-closed unknown disposition; disposition-uniqueness test; `aria-live` announcement names state + target; pulse animation suppressed under `prefers-reduced-motion`. |

### Hardening items found (P2/P3 — fold into this slice, do not file)

1. **No key-set pin for serialized `ComputerAuditEntry`.** #292 pins the
   observation key set so future fields cannot silently widen exposure; event
   pages (the other coordinator-visible payload) have no equivalent test. Add
   one.
2. **Recovery writes no audit entry.** `recover_interrupted` changes the run
   record but appends nothing to the journal, so the event stream alone never
   shows the interruption (the projection does). #286 expects restart to
   produce durable audit entries. Append an `("recover", "interrupted")`
   entry during recovery.
3. **Future cursor is indistinguishable from caught-up.** `after_seq >
   end_seq` returns an empty non-expired page. Harmless (bounded, no leak);
   document it in the tool description rather than change semantics.
4. **`capacity()` exposes global `stored_runs`/`active_runs` to any
   session-scoped caller.** Matches the global semantics of
   `ptah_get_capacity` (single shared bearer token; one trust domain); keep,
   but document as intentional.
5. `record_audit` trims only when `len == 1024` exactly; `>=` is more
   defensive (unreachable via store reads because `validate_run_record`
   rejects longer vectors, so cosmetic).

## 2. Workspace identity — verified absent; binding design

**Verified:** `ComputerRun` (`types.rs:616-676`) has no workspace field; all
creation sites pass only `owner_session_id`
(`desktop/src-tauri/src/computer_use.rs:190-196`, `:321-327`, `:379-384`);
no session→workspace derivation exists for computer runs. MCP authorization
therefore cannot copy the Build-run triple check today.

**Design (first commit of the slice):**

- `ComputerRun.workspace: Option<String>` with `#[serde(default)]` — the
  canonicalized workspace path string, stamped **at creation** and never
  rewritten. `recover_interrupted` leaves it untouched, so the binding
  survives restart by construction (add a test proving it).
- `ComputerRun::new` and `ComputerUseService::create_run` gain a
  `workspace: Option<String>` parameter. The desktop command layer resolves
  the owning session's cwd (it holds the `AgentHostHandle`; sessions carry
  `cwd`), canonicalizes with the same `dunce::canonicalize` path used by
  `orchestration::authz::canonical_workspace`, and passes it in. Do **not**
  resolve it inside the service from ambient process state.
- `validate_run_record`: when `Some`, require non-empty, ≤ 4096 bytes, no
  NUL.
- MCP reads **fail closed on the binding**: a run whose `workspace` is `None`
  (legacy record) or differs from the caller's canonicalized claim returns
  the same single `unauthorized` error as unknown/cross-session. No
  inference, no fallback to session cwd.
- GUI paths are unaffected (they never consult workspace).

## 3. Store sharing — how the control plane reaches the ledger

`ComputerStore::open` holds an fs2 exclusive lock (`store.rs:77`), so the
control plane can never open its own handle. Mirror the existing precedent
`AgentHost::ensure_orchestration_store` / `install_orchestration_store`
(`host.rs:712-736`):

- Add to `AgentHost`: `computer_store: Mutex<Option<ComputerStore>>`,
  `pub fn ensure_computer_store()` (opens `grokptah_home()/computer-use`),
  `pub(crate) fn install_computer_store(store)`, `pub fn computer_store()`.
- `DesktopComputerUse::new` takes the host handle and uses
  `ensure_computer_store()` instead of opening directly
  (`desktop/src-tauri/src/computer_use.rs:81`), so desktop and MCP share one
  locked handle. Desktop state construction in `desktop/src-tauri` passes the
  host it already owns.
- The four scoped reads in #292 touch only `self.store`, never the backend.
  Move their bodies onto a small `ComputerRunReads` wrapper over
  `ComputerStore` (same module); `ComputerUseService` delegates so #292's
  signatures, behavior, and parity test are unchanged. `dispatch_tool`
  reaches reads via `OrchestrationService.host` (`orchestration/service.rs:57`)
  → `computer_store()` → `ComputerRunReads`, with no backend and no second
  policy surface.
- Host without an installed/openable store (or non-desktop context): every
  computer tool returns `OrchError { code: Unsupported, message: "computer
  use is unavailable on this host" }` — global, session-independent, leaks
  nothing.

## 4. Tool contracts (registered through the existing plane only)

Registration points: `CONTROL_TOOLS` (`orchestration/types.rs:561`),
`tool_input_schema` + `dispatch_tool` (`mcp_control.rs:1198`, `:1413`),
schema/name snapshot tests (`tests/mcp_streamable_transport.rs:70`, `:446`;
`tests/orchestration_adversarial.rs:482`), docs
(`docs/MCP_CONTROL_COORDINATOR.md` tool table, `docs/COMPUTER_USE.md`,
`docs/TOOL_MATRIX.md`). No new transport, listener, port, or auth path.

| Tool | Required arguments | Returns (serde of #292 types) |
|---|---|---|
| `ptah_list_computer_runs` | `session_id`, `workspace` | `{ "runs": [ComputerRunProjection…] }` newest-first, session-scoped (ledger hard cap 256 records ⇒ response ≤ ~300 KiB) |
| `ptah_get_computer_run` | `session_id`, `workspace`, `run_id` | `ComputerRunProjection` (status, disposition/epoch, progress, observation metadata, grant summary, lastOutcome/lastError, eventRange) |
| `ptah_get_computer_run_events` | `session_id`, `workspace`, `run_id`; optional `after_seq` (int ≥ 0), `limit` (1–500, default 100) | `ComputerRunEventPage` (`runId`, `entries`, `nextCursor`, `cursorExpired:false`, `range`) |
| `ptah_get_computer_capacity` | `session_id`, `workspace` | `ComputerRunCapacity` (global `maxRunRecords`/`storedRuns`/`activeRuns` + per-session counts) |

Schemas follow the house style exactly: `additionalProperties: false`,
`session_id` uuid string, `workspace` non-empty string, `run_id` string
1–256; serde arg structs use `deny_unknown_fields`. Reads take no
`request_id` (nullipotent, like every `ptah_get_*`); duplicate JSON-RPC ids
are transport-level and already exercised.

**Authorization pipeline, in this order** (run-independent failures first so
run existence is never signaled):

1. Bearer auth middleware — already global, before body work (`mcp_control.rs:487-504`).
2. Parse args (`deny_unknown_fields`), `require_nonempty(run_id)`, limit
   bounds → `invalid_request`.
3. Session must exist (`host.session_load`) → mirror existing vocabulary.
   Computer runs are owned by build *and* chat sessions (cockpit copy:
   "Open a build or chat session"), so do **not** reuse
   `require_build_session`; require existence + cwd only, and record this
   deviation in the PR description.
4. `require_workspace_match(allowlist, session.cwd, claimed)` → allowlist +
   canonicalize + session-cwd match (`orchestration/authz.rs:105-131`),
   errors `workspace_mismatch` — still independent of any run.
5. Scoped read: `validate_id(run_id)` + load + `owner_session_id` match +
   durable `workspace` binding match → any failure is the single
   `unauthorized` error ("computer run is not available to this session").

**Error mapping** (`ComputerErrorCode` → `OrchErrorCode` → HTTP, one total
function): `Unauthorized` → `ForbiddenScope` → 403 with message passthrough
(preserves indistinguishability — one code, one message for unknown /
cross-session / cross-workspace / unbound / traversal); `InvalidRequest` →
`InvalidRequest` → 400; cursor below retained window → `CursorExpired` → 410
(`data.code: "cursor_expired"`, matching `ptah_get_events`; recovery path:
re-read the projection's `eventRange` and resume from `startSeq - 1`);
everything else → `Internal` → 500. When expiry maps to the 410 error, the
page's `cursorExpired: true` body is not returned — the error IS the signal,
keeping one recovery idiom across the plane.

**MCP-originated activity visibility (#286):** this slice adds no MCP
mutations, so there are no MCP-originated runs to label. Transport-level
read access is already durably audited (`audit_transport_result`,
`orchestration/service.rs:300-322`). The durable `origin` field on
`ComputerRun` plus cockpit labeling ships with the first mutation slice —
deliberately not a dead field now (matches the #271 comment). The cockpit
snapshot's `origin: "desktop" | "mcp"` discriminator already exists in
`protocol.ts` for that moment.

## 5. Tests

Rust unit (bridge `cargo test --lib`):
- workspace binding: stamped at creation, survives recovery, `None`-legacy
  and mismatched-workspace reads return the byte-identical unauthorized
  error as unknown-run; canonical string equality only.
- `ComputerAuditEntry` serialized key-set pin (hardening item 1).
- recovery audit entry appended (hardening item 2) and visible via
  `owned_run_events` after reopen.

Transport (`tests/mcp_streamable_transport.rs`, offline host + simulator):
- tools/list contains exactly `CONTROL_TOOLS` incl. the four new names, with
  `additionalProperties: false` and the required triple in each schema.
- auth-before-body on the new tools (malformed body without token → 401).
- unknown vs cross-session vs cross-workspace vs legacy-unbound run → same
  status, same `data.code`, same message (byte-compare the JSON-RPC error).
- cursor expiry → 410 `cursor_expired` after forcing ring eviction; exact
  continuity from `startSeq - 1`; limit 0 / 501 rejected.
- capacity figures; list scoping (other session's runs absent).
- fresh-host restart (mirror the soak pattern): reopen store, new control
  server, projection reports `interrupted`, events replayable via the tool.
- no-store host → `unsupported` for all four tools.
- parity: for a terminal run (no volatile fields), desktop
  `project_run_at` output equals the tool's `structuredContent`.

Release gate (`tests/computer_use_release_gate.rs`): snapshot that the MCP
surface gains **only** these four read tools — no computer mutation, shell,
raw input, screenshot, or evidence-byte tool names.

Node conformance (`tests/mcp_sdk_interop/run_conformance.mjs` + Rust driver):
discovery, scoped read, paging with strictly increasing sequences, expiry
handling, cross-session rejection through an independent client.

Frontend: no UI change in this slice (read tools only); #292's cockpit,
activity, and a11y suites remain the coverage. Desktop-started live smoke:
extend `run_live_smoke.mjs` with the four tools and run it via the
documented desktop env contract (`GROKPTAH_CONTROL_TOKEN`/`PORT`/
`WORKSPACES`) on a real desktop host — **this is the "live proof" step and
it has not been run**; it needs a desktop-capable machine.

## 6. Deliberately deferred (document in the PR)

- All Computer Run mutations over MCP (submit/steer/pause/cancel/take-over)
  and grant issuance with client identity — #271's grant section; the
  service-side idempotency receipts and takeover fences already exist.
- Durable `origin` + cockpit MCP-activity labeling (#286) — ships with the
  first producer.
- Live computer-run event streaming (`notifications/ptah_event` pattern) —
  the gap contract in #292 already matches `ptah_recovery` semantics.
- Bounded redacted evidence/screenshot resources (#271), background-safe
  tier (#287), isolated visual backend (#288).

## 7. Risks and open questions

- **Session-kind gate** (step 3 above): existence + cwd match, not
  Build-only — flagged for review since it deviates from Build-run reads.
- **Response envelope:** list tool ≤ ~300 KiB worst case (256 × ~1.2 KiB);
  acceptable now; an optional `limit` can be added compatibly.
- **Desktop plumbing:** `DesktopComputerUse::new` signature change ripples
  through `desktop/src-tauri` state setup; desktop crate cannot be compiled
  on this container (gtk/webkit2gtk system deps absent) — same limitation
  #292 documented; needs the macOS/desktop CI leg.
- **Pre-existing Build-run read oracle** (unknown → `invalid_request` vs
  cross-session → `forbidden_scope`, single shared token): out of this
  slice's scope; worth a separate issue if probe-hardening is wanted there.

## Storage blocker

This container's writable allowance is ≈ 37 GiB total with **30 GiB free at
session start** (`df`: 252G volume, 7.2G used, 30G avail — the allowance is
the binding constraint, not the volume). The goal mandates: below 50 GiB,
read-only audit/design only — no dependency installs, no builds, no native
tests. 30 GiB < 50 GiB, and the 50 GiB floor is **unsatisfiable by
construction** in this container class (allowance < floor even when empty).
The implementing session needs roughly 12–25 GiB free (bridge debug target
≈ 8–12 GiB, cargo registry ≈ 2–3 GiB, `desktop/node_modules` ≈ 0.6 GiB) plus
headroom — i.e. either a machine with ≥ 50 GiB free, or the floor relaxed
for containers with an explicit ~30 GiB budget.

Verification commands for that session (from `docs/MCP_CONTROL_COORDINATOR.md`
and `docs/VERIFICATION.md`, run from `crates/codegen/grokptah-agent-bridge`,
with a disk check before and after each):

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --lib
cargo test --test mcp_streamable_transport --test orchestration_adversarial -- --test-threads=1
cargo test --test computer_use_release_gate
cargo test
cd ../../../desktop && npm run typecheck && npm test
```
