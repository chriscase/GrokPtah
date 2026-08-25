# Grok Build runtime truth: launch authority and provider attempts

This document reproduces, in this branch, the authority requirements this
work was asked to satisfy. It is written from the requirements as stated to
this session; the manager worktree's own handoff document was deliberately
not read or modified.

Everything below is backend/runtime only. The desktop `App`, its components,
and the source viewer are out of scope, as are the protected soak,
external-worker, Semantic Help, and Computer Use subsystems.

## 1. What the contracts answer

Three contracts, each answering exactly one question, each fail-closed.

| Contract | Question | Module |
| --- | --- | --- |
| `grokptah.account.v1` | Is there a credential, and has it visibly expired? | `grokptah_agent_sdk::account` |
| `grokptah.launch.v1` | May this host admit a durable run right now, and against what? | `grokptah_agent_sdk::launch` |
| `grokptah.attempt.v1` | Did this request reach the provider, and is it safe to send again? | `grokptah_agent_sdk::attempt` |

The account contract is deliberately permissive: an unknown fact never blocks.
The launch contract inverts that. **Unknown, unrecognized, unparseable, and
unprobed all refuse.** The one exception is stated positively rather than
inferred: a resolved API-key route reports
`resolved_api_key_no_expiry_claim`, which says "this route carries no expiry
claim at all", not "expiry is unknown". Only `api_key`, `provider_env`,
`provider_keychain`, and `grok_build_api_key` may make that claim; a session
route with no expiry is `expiry_not_established` and refuses.

## 2. What a launch decision is made of

`GrokLaunchTruth` carries a closed, secret-free vocabulary for every fact a
launch depends on:

- **provider** (`ProviderClass`) — family, never a user-authored profile id
- **credential method** (`CredentialMethod`) — closed, no `Other(String)`
- **expiry** (`ExpiryFacts`) — re-serialized from parsed components
- **refreshability** (`Refreshability`) — read from the route's refresh
  machinery, never from a token body
- **route class** (`RouteClass`) and **base category** (`BaseCategory`) — the
  endpoint is classified, never published: it can embed a private hostname, a
  tenant name, or a path secret
- **dialect** (`RequestDialect`) and **capabilities** (`CapabilityFacts`) —
  with provenance, so `unprobed` asserts nothing
- **model** (`ModelFacts`) — bounded, charset-limited, traversal-shaped values
  refused
- **reason** (`LaunchReason`) — 23 values, every blocking one naming an
  operator action. There is no catch-all.

`readiness` is `ready | blocked | indeterminate`, and only `ready` permits a
launch. `blocked` is positive evidence; `indeterminate` is an unestablished
fact. Both refuse, and they are distinct so an operator can tell "this is
wrong" from "this is unknown".

## 3. Where the gate lives

The gate is **host-owned**: `AgentHostHandle` holds one
`Arc<dyn LaunchGate>`, fixed at construction, defaulting to the production
`HostLaunchGate` that re-resolves and refreshes live local credentials.

Every provider-bound turn funnels through `session_prompt_inner`, which
consults that gate. That single choke point covers:

- the desktop Send path and the Tauri `session_prompt` command,
- `session_prompt_with_max_rounds` and the reserved-turn variants,
- queued and steered prompts, which re-enter through the same functions,
- plan-mode resume,
- coordinator-owned runs over MCP, which additionally re-enforce against the
  exact facts they were admitted on,
- **Chat sessions as well as Build sessions** — a Chat turn spends a
  credential exactly like a Build turn does.

`OrchestrationService` reuses the host's gate rather than building its own, so
a coordinator submission and a desktop turn can never be admitted under
different policies. Compaction is gated too; it degrades to its deterministic
local summary rather than failing the operator's compaction.

An offline host (`GROKPTAH_AGENT_OFFLINE`) reaches no provider at all: the
turn runner short-circuits to a stubbed turn *before* resolving any
credential. The gate reads that same switch, in one place, and returns
`Admission::NoProviderReachable`, which records **no attribution** — claiming
one would attribute a run to an account it never touched.

## 4. Re-resolution before durable admission

Before anything durable exists, the host re-resolves and refreshes:

1. `OrchestrationService::submit_task…` calls the gate **before** capacity
   reservation and before `save_run`. A refusal writes no run record at all.
2. The admitted facts are pinned onto the record as
   `RunRecord.launch_requirement` and `RunRecord.attribution`. Both are
   **write-once**: `OrchStore::update_run` refuses any mutation that changes
   either, and a rejected update leaves the record on disk untouched.
3. `spawn_run` re-resolves again immediately before the model turn and
   enforces against the pinned requirement, because a queued run can wait
   arbitrarily long between admission and start.
4. A drift is recorded as a typed non-success state. It is never `completed`.

The credential resolved is the one the **selected model** belongs to
(`resolve_wire_credentials_for_model`), not the built-in xAI profile. A
compatible-provider host previously projected as "no credential"; that is
fixed and covered by `bridge_lifecycle::compatible_model_completes_…`.

## 5. Typed terminal outcomes

`grokptah_agent_sdk::outcome` maps every observable failure onto one of
`blocked | failed | indeterminate`, and `RunOutcomeClass::state()` has **no
arm producing `Completed`**. Missing, revoked, and expired credentials,
refresh failure, route and model mismatch, `401`, `429`, other provider
errors, transport failure, and malformed output all land there.

Transcript help is explicitly preserved: `retains_transcript_help()` is always
true. The state is the lie that is prevented, not the explanation.

## 6. The send boundary

`ProviderAttempt` records what each request was bound to and whether it left:

```
KnownNotSent ──► Sending ──► Sent
                       └───► Uncertain
```

**Only `KnownNotSent` may auto-retry.** An interrupted `Sending` is
indistinguishable from a delivered request without asking the provider, so it
is not auto-retryable however much it looks like one that never left. The
lattice is strictly forward; nothing rewinds into a retryable state, and the
durable store enforces that independently of the in-memory type.

Bound into every attempt, write-once:

- **subject** — principal (or an explicit "none published" for a bare API-key
  route), tenant, project, workspace, session. All opaque and bounded; the
  workspace is a digest, never a path.
- **authority revisions** — auth, policy, capability, credential. Recorded so
  a superseded decision is detectable after the fact.
- **route** — provider, profile, credential method, route class, base
  category, dialect, model, effort, account reference.
- **intent** — an opaque digest of the request (never the text), the caller
  request id, and the **provider idempotency key**, derived from run and
  ordinal so a restarted host reproduces it exactly.
- **receipts** — provider request id, run id, token counts, and whether a
  complete parseable reply arrived.

Cancel and restart **reconcile the exact attempt**: cancelling moves an
in-flight attempt to `uncertain` rather than un-sending it, and `retry_run`
refuses while any attempt is unreconciled, naming the idempotency keys the
operator needs to settle it.

## 7. What is never claimed

No contract here carries a bearer, refresh token, API key, keychain
reference, credential fingerprint, endpoint URL, hostname, email, display
name, prompt text, or response body.

A local timestamp is evidence about *this host's record-keeping*. It is never
called token-ready, entitlement, quota, balance, or billing: only a provider
round-trip can establish those, and none of this code performs one.
`UsageReceipt` is counts only.

## 8. Isolation and rollback

New Build sessions on a **clean Git workspace default to an isolated
worktree**, the only mode whose changes are reviewable before they touch the
operator's checkout. Where isolation cannot be prepared (a dirty or non-Git
workspace), the session falls back to shared rather than being created in a
mode whose first turn would fail. The default follows the workspace when a
session is rebound; an explicit operator choice is never recomputed.

Shared execution is an **explicit unsafe opt-in** where isolation was
available, and it states its guarantee:
`RunExecutionMode::Shared.rollback_guarantee() == RollbackGuarantee::None`.
Review and promotion already refuse shared runs, so any rollback claim would
be one the runtime cannot honour.

## 9. Strict schema peers

`crates/common/grokptah-agent-sdk/tests/schema_peers.rs` runs a **real Draft
2020-12 validator** (`jsonschema`) over:

- the golden **accepted** fixtures, which must validate;
- the golden **rejected** fixtures, which must be refused by the schema, the
  strict Rust decoder, or the Rust validator — sailing past all three is the
  only forbidden outcome;
- **real producer payloads** serialized from the Rust types, which no fixture
  can substitute for: a projection the schema would reject is a published
  contract nobody downstream can parse.

Closed vocabularies are checked in both directions, and the bounded-identifier
and model-id patterns are asserted to accept and refuse exactly what the Rust
constructors do. That check already caught one real drift (the schema pattern
allowed `..` where Rust refused it).

## 10. Known gaps

These are listed rather than papered over.

- **TypeScript peer and the account badge.** The desktop `App`, components,
  and source viewer are out of scope for this branch, so the TS strict peer
  for `grokptah.launch.v1` / `grokptah.attempt.v1`, and the DOM/accessibility
  work for opaque account ids and live blocking announcements, are not done
  here.
- **Authenticated browser broker parity.** The MCP control plane is gated
  through the shared host gate, but no separate broker production-parity
  suite exists in this branch.
- **Live provider evidence.** No live credential was used. Readiness, send,
  cancel, and restart are proven against the fake compatible gateway and the
  durable ledger, not against a live sandbox.
- **Authority revisions are recorded, not yet sourced.** There is no versioned
  auth/policy/capability decision store to read from, so attempts record the
  initial revision. The binding and the comparison are in place; wiring real
  revisions is a follow-up.
- **Provider request/run ids.** The host does not yet surface them, so an
  acknowledged attempt records `providerReplied` plus token counts. The
  receipt fields exist and validate.
