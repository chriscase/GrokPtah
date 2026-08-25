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

### What the previous revision actually did

The independent review was right, and the code agreed with it:

- The gate was a **turn-level precheck**. It ran once, recorded the session's
  model and effort at that instant, and then the transport re-resolved
  credentials, provider profile, base endpoint, and model again just before
  sending — and again on every retry and after every 401 refresh. Three
  readings, no guarantee any two agreed, and the record described the first.
- `call_xai_agent_step` contained a four-iteration retry loop **and** a
  401-refresh resend, so one `ProviderAttempt` could stand for five physical
  requests, each independently capable of having executed.
- The idempotency key was derived and recorded but **never transmitted**.
- The intent digest covered the **prompt only** — not the system preamble, the
  history, the tool declarations, the model, or the effort.
- The retry path **mutated the request body in place** (dropping `tool_choice`,
  disabling `stream`), so the ledger described a body that was never sent.
- Desktop run persistence was best-effort: a ledger failure was printed and
  the turn continued unrecorded. Chat turns had no run at all.
- A provider, auth, or transport failure in a Chat turn was formatted as
  **ordinary assistant text**, making a failed turn indistinguishable from a
  successful one.

### What replaces it

`grokptah_agent_sdk::resolved::ResolvedRequest` is an immutable carrier for
**one physical request**. It holds the exact bytes that will be transmitted
alongside the complete binding they were resolved under, and it is:

- **not `Serialize`** — it holds request bytes, which contain user content.
  Only its `RequestBinding` is persistable, and that carries a digest.
- **self-digesting** — `seal` computes the digest from the bytes it is handed.
  A caller never supplies a digest, so bytes paired with some other request's
  digest is not expressible.
- **complete** — the digest covers the whole canonical body: system preamble,
  history, tools, model, effort, and every other field.

The binding carries principal, tenant, project, workspace, and session; the
provider, the **exact selected profile** (not one inferred from the family),
an `EndpointIdentity` that pins the exact base URL by fingerprint without
publishing it, the route, the dialect, the exact wire model, the exact effort,
the credential method **and its revision**, the decision revisions, the
digest, the body length, and the source revision.

`request_admission::admit_call` is the only thing that mints one. It resolves
the credential, enforces launch truth, resolves the route, builds the body, and
seals — once, in that order, with nothing re-resolved afterwards.

`provider_transport::send_admitted` is the only place a provider request
physically leaves. It takes an `AdmittedCall` and has no other way to learn a
URL, a credential, or a body, so an unadmitted send does not typecheck. Its
ordering is:

```text
verify bytes match the sealed digest
persist attempt as known_not_sent   ── crash here: safe to retry
persist attempt as sending          ── crash here: never auto-retried
.send()  (with Idempotency-Key)     ── the request may now exist
settle sent / uncertain
```

Every one of those ledger writes **fails closed**. A request that cannot be
recorded is not sent.

One call to `send_admitted` is exactly one HTTP request. A retry is a new
admission with a new digest, a new ordinal, and a new key, and it only happens
when the ledger says the previous request provably never left. Narrowing a
request (dropping `tool_choice`, disabling streaming) **re-admits and re-seals**
rather than editing a recorded body. A 401 refresh rotates the credential,
advancing `credential_revision`, and resends as a separate recorded request.

### Coverage

Every provider call site now goes through it, enforced by the compiler:
`run_turn` (Build and Chat), the coding-agent loop, plan proposal, compaction,
the explore subagent, the GP subagent, and Computer qualification and
proposal. `call_xai_chat` and `call_xai_agent_step` take a session id rather
than a credential, and a session with no registered provenance is refused —
so forgetting to register fails closed rather than sending unrecorded.

Every provider-bound turn now gets a durable run, **Chat included**, and
`begin_desktop_run` returns an error rather than `None` when the ledger is
unavailable. A Chat provider failure now propagates instead of becoming
assistant text.

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

## 10. Residual risks

Stated plainly, because a review that has to find these itself is a review
that cannot trust anything else here.

### Still bypassing the send boundary

- **`provider_qualification.rs` is not admitted.** It posts to
  `chat/completions` directly and can issue up to five physical requests per
  probe against a live credential, with no run, no attempt, no transmitted
  key, and no fail-closed ledger. It has a real ordering problem the rest of
  the boundary does not: admission refuses a model whose capabilities are
  unprobed, and this *is* the probe that establishes them. Fixing it needs a
  distinct qualification admission. The `no_unadmitted_provider_calls` test
  asserts this list matches the tree **exactly**, so the gap cannot widen
  silently, but it is open today.

### Bound but not yet sourced

- **Authority revisions are placeholders.** `auth`, `policy`, and `capability`
  all record the initial revision, because no versioned decision store exists
  to read from. The binding and the comparison are real; the values are not
  yet meaningful.
- **`credential_revision` is process-local.** It advances when the bearer
  changes, which answers "did the credential rotate between admitting and
  sending this request". It restarts at zero and is not a durable rotation
  count.
- **Tenant and project are always `None`.** There is no store to resolve them
  from, so the binding records "none established" rather than inventing one.
- **Provider run ids are not captured.** Only a request id from response
  headers, plus token counts. `receipts.run` is always `None`.

### Not attempted in this branch

- **TypeScript, Tauri, broker, and public-package conformance.** The strict
  peer suite covers Rust against the published JSON Schemas with a real Draft
  2020-12 validator. There is no TS peer, no Tauri-command conformance, no
  authenticated-broker production-parity suite, and no public-package check.
- **Action-time accessible UI truth.** No work on keeping opaque account ids
  out of the DOM and accessibility tree, and none on announcing live blocking
  transitions.
- **Promotion and ledger TOCTOU.** Pathname-based checks remain; no no-follow
  or handle-based hardening was done.
- **Dirty and non-Git workspaces still fall back to Shared silently.** The
  fallback is deliberate — isolation cannot be prepared there — but it is not
  surfaced to the operator as a high-risk choice at action time.
- **Live provider evidence.** No live Grok Build sandbox credential was used.
  Readiness, send, cancel, and restart are proven against a real local HTTP
  server and the real durable ledger, not against a live provider.

### Environment caveats for the evidence below

This branch was built and tested on **Linux x86_64**, not macOS. `sccache` is
not installed in that container and was not used; builds ran against a
lane-namespaced external target directory. Two `clippy` findings in
`computer_use/macos_observation.rs` are dead-code false positives on Linux —
the code is consumed under `#[cfg(target_os = "macos")]` — and do not fire on
the macOS CI runner.
