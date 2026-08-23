# Persistent-Agent certification lab

The Live Grok Persistent-Agent Certification Lab is a second consumer of the
public Streamable HTTP/MCP contract. Its owned local mode constructs the same
bridge host and control server in a disposable home, but every probe action and
oracle goes through `McpControlClient`; it does not inspect the orchestration
store. Attach mode consumes an already-running local or hosted service.

The executable is the standalone Rust workspace in
`evals/certification-lab`. The authoritative promotable scenario taxonomy and
capture schema remain `evals/persistent-agent-scenarios.v1.json` and
`grokptah_agent_bridge::certification::PersistentAgentCapture`. The lab's
`campaign.v1.json` is only an execution overlay: its stable probe IDs point to
existing catalog IDs and declare tools, capabilities, bounds, transitions,
actions, and typed oracles.

The hardened capture discriminator is
`grokptah.persistent_agent_capture.v2`: v2 adds mandatory opaque campaign
identity, opaque durable Agent/lane/Run/checkpoint identities, and authoritative
bound-profile fields. Old or unknown capture schemas fail closed; the
independent scenario catalog remains v1.

The v2 capture remains backwards-readable for the original single-Run shape.
New recovery captures use `durableStates[].role` to distinguish the completed
`provider_capture` Run whose sanitized provider observations are retained from
one or more `recovery` Run states whose durable lifecycle is under test. A
recovery capture must pass explicit partition and no-implicit-resume checks;
it is evidence for review, not an automatically promotable replay fixture.

## What it proves

The deterministic smoke campaign proves, through the public MCP surface:

- service readiness against the actual `ptah_get_capacity` shape and exact
  `grokptah-control` MCP identity;
- durable Agent identity projection after a finite one-round offline Run;
- Work request replay plus a typed changed-payload conflict and an independent
  durable reread;
- exact manual Routine activation linkage; and
- independently reread coordinator parent/child Work lineage;
- checkpoint inspection, explicit same-Lane continuation, and idempotent
  continuation replay; and
- native managed execution remaining disabled by default, with a healthy
  executor projection.

The checked structural replay fixture covers normal completion, a tool call,
malformed tool arguments, provider rejection, timeout, interruption, explicit
rate-limit backoff, duplicate suppression, and permission allow/deny. These
are synthetic behavior fixtures. A normalized live capture is explicitly
`live_capture_structural`: it can retain only attempt classification, framing,
usage shape, one opaque Run partition, and a typed durable terminal. It cannot
reconstruct provider SSE payloads, model prose, tool arguments, or reasoning.
Recovery captures may additionally retain opaque structural states for the
interrupted or explicitly retried Run without attaching that Run's provider
payload to the successful provider-capture attempt.

The lab does not claim model quality, exact prose, unattended long-lived model
execution, or Computer Use safety. The merged service exposes native managed
execution; the default smoke campaign verifies that the policy is disabled by
default. The native Work-to-Run, permission, duplicate-suppression,
restart-adoption, and interruption-retry probes are implemented for bounded
live evidence, but remain capability-gated until an explicitly authorized live
campaign runs them. Live model quality remains
nondeterministic and is never a deterministic CI gate.

## Safe commands

From the repository root:

```sh
cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  validate-manifest --repository "$PWD"

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  replay --fixture "$PWD/evals/certification-lab/replay-fixtures/provider-behaviors.v1.json"

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  preflight --repository "$PWD"

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  run --repository "$PWD"
```

### Exact-head Stage 5 memory campaign

The `memory` command is the deny-unknown integrated exit runner for roadmap
stage 5. It requires one clean Git checkout and binds the report to that exact
40-character commit. It executes the ordered logical-years, crash/cutpoint,
compaction/reopen, cross-process restart, scope-isolation, Manager attribution,
Manager objective, Manager-store restart, supervisor loopback, and native
proposal gates. A missing, reordered, failed, cardinality-mismatched, or
candidate-drifted gate leaves an incomplete campaign and cannot create a
completion seal or certification claim.

Run it serially with the pinned shared compiler cache and a compatibility-keyed
external target. These variables are mandatory; do not create another target
inside the checkout:

```sh
export RUSTC_WRAPPER=/opt/homebrew/bin/sccache
export SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
export CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  memory --repository "$PWD"
```

Owner: the single Stage 5 campaign process. Reuse that target only for this
Rust 1.92.0/default-feature family and never concurrently. Before the command,
record disk headroom and verify that no other process owns the target. After
the command, record the target size, cache statistics, and owner state. Retain
the shared target for the next serial compatible run; delete only a separately
named incompatible/concurrent target after final process and open-handle
checks.

A passing run writes a sealed report beneath
`evals/runs/memory-stage5-cert/<campaign-id>/`. Independently verify it with:

```sh
cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  inspect --campaign "$PWD/evals/runs/memory-stage5-cert/<campaign-id>"
```

The sealed report retains command/output digests and exact test cardinalities,
not raw transcripts or credentials. This accelerated logical-years campaign is
separate from the stage-6 72-hour operational soak.

The default run selects the seven deterministic probes above, creates a
disposable runtime home and workspace, uses the offline provider path, and
writes under `evals/runs/persistent-agent-cert/<campaign-id>/`. Override
manifest, fixture, campaign, capture, and output paths must be absolute. An
output inside the source repository is accepted only beneath that precise
ignored campaign root; an output outside the repository must still be an
absolute sentinel-owned root.

The owned-local restart probe can be run independently when restart durability
is the focus:

```sh
cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  run --repository "$PWD" \
  --probe core-restart-durable-runs-events-v1
```

It creates a bounded offline Run, closes the MCP session, restarts the same
durable local home, reconnects through a new MCP session, and compares the
post-restart Run projection and event sequence. It does not claim that an
interrupted model invocation resumed.

Inspect a completed report without reading transcripts:

```sh
cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  inspect --campaign "$PWD/evals/runs/persistent-agent-cert/<campaign-id>"
```

Create a review candidate from a bounded structural capture:

```sh
cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  normalize --repository "$PWD" \
  --capture "/absolute/path/to/capture.json" \
  --output "$PWD/evals/runs/persistent-agent-cert"
```

Normalization requires a clean public-xAI capture, all-passing structural
checks, explicit complete-observer and attempt-to-Run binding checks, exactly
one durable Run, and no payload artifact references. The output is a sealed
review candidate with `automatic_promotion=false`,
`fixture_promotion_eligible=false`, and `human_review_required=true`. The
command never copies into a versioned fixture directory, commits, or promotes
anything.

Retention is explicit and never runs as part of a campaign:

```sh
cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  prune --repository "$PWD" \
  --output "$PWD/evals/runs/persistent-agent-cert" --keep 5
```

Zero retention is rejected. Pruning considers only completed direct children
whose ownership marker, completion seal, final-artifact digest, and bounded
tree validate; unknown, incomplete, active, malformed, or symlink-containing
children are preserved.

## Attach mode

Attach endpoints reject userinfo, query strings, fragments, redirects, and
non-root base paths. Plain HTTP is allowed only for literal `127.0.0.1` or
`::1`; all other endpoints require HTTPS. Requests have a fixed timeout and
bounded response/SSE parsing. Remote prose and bodies are mapped to typed
diagnostics and never enter reports.

The token is supplied only through an environment-variable name, never as a
CLI value:

```sh
export GROKPTAH_CERT_SERVICE_TOKEN='...'
cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  preflight --repository "$PWD" \
  --attach https://private-service.example \
  --probe core-service-readiness-v1
```

Attach is read-only by default. The service does not expose a stable
deployment identity plus a service-issued campaign-specific disposable
workspace lease, so mutating attach is refused even if the caller supplies the
mutation and disposable-target flags. Attach also has no observer attestation;
it cannot produce passing provider-certification evidence.

## Live Grok/xAI safety gate

A live request requires both `--live` and `GROKPTAH_LIVE_CERT=1`, plus an
explicit public `--model grok-*`. Those switches are necessary but not
sufficient. The production bridge now exposes a fail-closed, positive-schema OIDC
attestation and a bounded secure-refresh path, documented in
[`GROK_BUILD_LIVE_ATTESTATION.md`](GROK_BUILD_LIVE_ATTESTATION.md). It refuses
API-key, keychain-key, token-command, compatible-gateway, endpoint, unsafe-file,
issuer, and model ambiguity. The official `GROK_HOME` cache-location override
is honored consistently. The campaign gate requires the cached session to
outlive the selected campaign bound, so the first live path does not depend on
an invented OAuth client-ID or refresh policy.

Live execution may be enabled only after an OIDC-only resolver attests all of:

- Grok Build OIDC credential class;
- endpoint `https://cli-chat-proxy.grok.com/v1`;
- the exact requested public `grok-*` model;
- absence of API-key, keychain-key, token-command, and base-URL overrides;
- safe allowlisted refresh issuer/token origins and no-follow, locked private
  credential storage; and
- matching post-call provider observation for route, credential, and model.

Live attestation validity is computed from the selected probe bounds. The
bridge retains the conservative maximum when xAI reports total tokens that
include accounting categories outside the visible prompt/completion pair, and
still rejects missing or under-counted usage.

Missing authoritative usage or any provider-observer drop makes dependent
claims indeterminate. Observation presence alone never assigns a provider
identity. The bounded-run probe now converts the selected finite Run and its
metadata-only provider observations into a payload-free
`grokptah.persistent_agent_capture.v2` artifact. Live execution remains a
separate explicit step; local validation does not claim that a model call was
made.

## Live campaign handoff

Run the live campaign from a user-facing Terminal that can both bind a
loopback listener and reach `cli-chat-proxy.grok.com`. The shell sandbox used
by some development agents may refuse the loopback bind before the first
provider request; that result is a harness failure, not provider evidence.

From the certification worktree:

```sh
cd /Users/chriscase/Documents/GitHub/GrokPtah/.worktrees/cert-codex
export GROKPTAH_LIVE_CERT=1

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  preflight --repository "$PWD" --live --model grok-build

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  run --repository "$PWD" --live --model grok-build \
  --probe core-bounded-run-terminal-v1

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  run --repository "$PWD" --live --model grok-build \
  --probe core-continuation-resume-v1

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  run --repository "$PWD" --live --model grok-build \
  --probe native-work-to-run-v1

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  run --repository "$PWD" --live --model grok-build \
  --probe native-permission-park-decisions-v1

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  run --repository "$PWD" --live --model grok-build \
  --probe native-no-duplicate-run-v1

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  run --repository "$PWD" --live --model grok-build \
  --probe native-restart-intent-adoption-v1

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  run --repository "$PWD" --live --model grok-build \
  --probe native-interruption-retry-policy-v1
```

The preflight must pass OIDC attestation, public-model validation, and service
startup. Each passing live campaign must produce a report plus a payload-free
capture artifact whose provider route, model, usage, Run identity, and bounds
all match. Inspect the sealed report with the `inspect` command before
considering it certification evidence. Never paste the Grok auth cache, bearer
token, request body, model prose, or raw provider stream into an issue or
review. Run the restart and interruption/retry probes as separate campaigns;
each owns one bounded local restart and must not be combined with another
restart-dependent probe in the same runtime home.

## Evidence, interruption, and privacy

Each campaign has a unique directory, a private ownership sentinel, one
exclusive lifetime OS lock, a byte budget that counts torn internal files by
unique inode, atomic same-parent writes, and a completion marker sealing a
verified final artifact. Reports contain the exact checked manifest artifact,
the exact selected probe set, typed action/oracle evidence, opaque hashes of
durable IDs, bounded counters, trace/capture hashes, and redaction/truncation
metadata. A report with omitted/extra probes, all skipped probes, a dirty
repository, missing provider capture, incomplete usage, recorder drops, or
inconsistent counts is never marked certified.

Once its campaign directory exists, the runner atomically refreshes
`campaign.partial.json` before and after probe boundaries. A first interrupt
returns exit 5; its output states that partial metadata is recoverable only if
campaign initialization had completed. Action-level resume is deliberately
disabled in v1 because safe resume still needs immutable prewritten request
IDs and reconcile-after-commit logic. The partial record says
`resume_supported=false`, cannot be sealed by a reopened writer, and is not a
final report.

Serialized evidence is positive-schema only. It rejects bearer/API/OIDC
tokens, embedded `sk-`/`xai-` credentials, JWT shapes, high-entropy token
candidates, secret JSON keys, URLs, absolute user paths, arbitrary MCP errors,
model output/reasoning, binary output, and unbounded records. Raw credential
contents and environment values are never read into a report.

The artifact implementation uses component-wise no-symlink checks,
canonical containment, private modes, exclusive locks, and opened-file
same-inode verification. It does not claim protection against a hostile
same-user process that can continuously replace directory ancestors between
pathname operations on platforms without handle-relative no-follow APIs.
Certification output must therefore remain in its private disposable root;
such an attacker is outside the v1 confinement claim.

## Result and exit meanings

- `passed`: the typed probe oracle passed with its declared tools, actions,
  transitions, trace, and evidence bounds.
- `failed`: an available contract produced contrary durable or protocol
  evidence.
- `skipped`: a declared tool/capability is unsupported on this runtime.
- `indeterminate`: the runtime is supported but required usage, observation,
  permission elicitation, deterministic clock, or other evidence is absent.

Process exits are: 0 exact clean selected contract certified; 1 oracle
failure; 2 invalid configuration/manifest; 3 skipped, indeterminate, dirty, or
required evidence unavailable; 4 safety/redaction/tamper/wrong-target refusal;
5 graceful interruption with partial metadata; and 6 harness/transport/I/O
failure. A second forced interrupt may terminate with the platform's standard
130 exit before cleanup.

## Coverage on the merged native-executor main

| Family | Current deterministic status | Notes |
| --- | --- | --- |
| Readiness/capabilities | Implemented | Actual capacity fields and persistence/supervisor error slots are checked; `nativeExecutor` is optional. |
| Agent identity | Implemented | Two MCP projections after one finite bounded offline seed Run. |
| Bounded live Run, steer/cancel | Declared, indeterminate without attested live evidence | Never inferred from a service-only Run. |
| Reconnect/restart/checkpoint | Reconnect, owned restart, checkpoint inspection, and explicit continuation implemented | Restart rereads the same terminal Run and event cursor through a fresh MCP session. Service-owned finite Runs persist a verified Agent checkpoint; continuation remains an explicit new finite Run with parent lineage and idempotent replay. |
| Work | Idempotency/conflict and lifecycle/lease implemented; dependencies and approval declared | Deterministic clock-dependent expiry remains skipped. |
| Routines | Manual activation implemented; schedule/dedupe/lifecycle/recovery declared | Scheduled timing requires deterministic clock support. |
| Coordinator/messages | Parent/child implemented; workers/messages/cursors/expiry/scope declared | Fixed message expiry without clock control is skipped. |
| Native managed execution | Default-off policy, Work-to-Run, permission parking, duplicate suppression, restart adoption, and interruption retry implemented | Public discovery, default policy, and `nativeExecutor` health are verified. All five live probes use public MCP oracles; restart/retry additionally require an owned local restart and explicit reconnect. Payload-free capture is bound to a successful provider-capture Run, while interrupted/failed scenario Runs are now retained as explicitly labelled recovery evidence. |
| Manager plans | Plan lifecycle implemented | One offline vertical slice: creation, a non-executable root container, dependency-ordered advance, revision-fenced observation, a failed step, and an explicit replan that reaches terminal success. |
| Soak | Declared, bounded, not implemented by the minimal runner | Existing coordinator/continuity/soak suites remain complementary evidence. |

On the merged native-executor main, discovery looks only for the audited public tools
`ptah_set_managed_execution`, `ptah_get_managed_execution`,
`ptah_authorize_work_execution`, `ptah_resolve_work_input`, and
`ptah_list_execution_intents`, plus `health.nativeExecutor.enabled=true` from
`ptah_get_capacity`. Even then, an unimplemented probe is indeterminate, not a
pass. Permission checks must use a real elicited permission bound to its exact
Run and are live-only: one fresh Work/Run/permission case is explicitly allowed
and a second independent fresh case is denied. The lab waits only within a
bounded capability window (120 public inbox polls); if the provider does not
elicit both permission cases, it cancels the disposable Runs and records
`permission_capability_absent` as skipped rather than consuming the full
campaign timeout. Each pre-execution-gated Work is authorized through
`ptah_authorize_work_execution`; a wrong, dead, or missing permission
resolution must fail closed and leave its Work parked. A single permission is
never treated as having transitioned to both decisions. The synthetic replay
fixture remains the deterministic unit-test path for allow/deny behavior.

The `retryEligible=false` probe observes one native interruption and its original
failed attempt, calls `ptah_retry_work`, proves repeated native ticks leave the
Work queued without attempt 2 or another native Run, and then proves an
external worker can claim it. `ptah_get_work` supplies the public attempt list
and linked Run IDs; no nonexistent attempt-list tool is invented. The probe
does not declare `interrupted -> retrying`.
Restart asserts only the public convergence—original Run interrupted, attempt
expired, Work failed, intent finalized, reconnect completed, and no duplicate
or implicit resumed Run. There is no public internal crash-stage/oneshot
injection seam, so adoption-journal stages are not claimed. Request replay and
repeated-tick no-duplicate evidence counts the public Work, attempts, intent,
and linked Runs; it does not invent a `deduplicated` Run state. The
newest-200-of-500 message prompt assembly is likewise not externally observable
and remains indeterminate until a public evidence seam exists.

### Manager plan coverage

`manager-plan-lifecycle-v1` is the first manager probe. It runs offline
against the local service and asserts only public MCP evidence:

- The plan's root Work reports `isContainer` and refuses a claim, so the
  coordination container can never execute. This is asserted against the
  host's refusal, not against prompt text.
- The first `advance` materializes only the step whose dependencies have
  succeeded; the dependent step stays `pending`. Replaying that one advance
  request returns the same Work rather than a second item.
- An `advance` carrying a superseded plan revision is refused.
- `ptah_tick_manager_plan` projects the terminal child outcome into one
  durable notification, and repeating the tick notifies nothing further for
  the same Work revision. The tick advances the active plan before it
  observes, so the dependent step's Work is read from the durable plan
  projection rather than from a second `advance`.
- A failed child leaves the plan `needs_replan` and creates no replacement
  Work. Only an explicit `ptah_replan_manager_plan` supersedes the failed step
  and its blocked descendants, after which the plan reaches `succeeded`.

The probe does not exercise the autonomous supervisor, `manager-decision`
Runs, or proposal-only capability enforcement; those need native managed
execution and remain uncovered.

## Adding a probe or fixture

Add a subordinate probe to `campaign.v1.json`, reference only authoritative
catalog IDs, declare every setup/action tool, make it self-contained, choose
the authoritative bound profile, and add an exact implementation mapping or
accept an honest unsupported/indeterminate result. Manifest tests reject
duplicates, unknown IDs/schema, undeclared tools, unsafe action ordering, and
inconsistent bounds.

Synthetic behavior fixtures may model tool/delta/permission behavior. A live
normalizer may emit only structural attempt/usage/terminal records. Replay
fails closed on unknown schema, malformed JSON, causal mismatch, noncontiguous
ordinals, implicit rate-limit backoff, ambiguous terminal cause, and unknown
catalog IDs. Fixture promotion remains a separate human-reviewed copy and
commit outside the runner.
