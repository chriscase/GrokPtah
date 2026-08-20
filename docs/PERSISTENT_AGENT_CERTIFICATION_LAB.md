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

## What it proves

The deterministic smoke campaign proves, through the public MCP surface:

- service readiness against the actual `ptah_get_capacity` shape and exact
  `grokptah-control` MCP identity;
- durable Agent identity projection after a finite one-round offline Run;
- Work request replay plus a typed changed-payload conflict and an independent
  durable reread;
- exact manual Routine activation linkage; and
- independently reread coordinator parent/child Work lineage.

The checked structural replay fixture covers normal completion, a tool call,
malformed tool arguments, provider rejection, timeout, interruption, explicit
rate-limit backoff, duplicate suppression, and permission allow/deny. These
are synthetic behavior fixtures. A normalized live capture is explicitly
`live_capture_structural`: it can retain only attempt classification, framing,
usage shape, one opaque Run partition, and a typed durable terminal. It cannot
reconstruct provider SSE payloads, model prose, tool arguments, or reasoning.

The lab does not claim model quality, exact prose, unattended long-lived model
execution, or Computer Use safety. The merged service exposes native managed
execution, but the lab's six native probes intentionally remain unimplemented
and therefore indeterminate rather than passing. Live model quality remains
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

The default run selects the five deterministic probes above, creates a
disposable runtime home and workspace, uses the offline provider path, and
writes under `evals/runs/persistent-agent-cert/<campaign-id>/`. Override
manifest, fixture, campaign, capture, and output paths must be absolute. An
output inside the source repository is accepted only beneath that precise
ignored campaign root; an output outside the repository must still be an
absolute sentinel-owned root.

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
sufficient. The current credential precedence can select an API key,
keychain entry, token command, or endpoint override, and its OIDC refresh path
does not yet provide the issuer/origin and storage guarantees required by this
lab. Therefore live preflight fails with the typed safety refusal
`live_oidc_route_attestation_unavailable` before starting a host or making a
network request. `XAI_API_KEY`, `XAI_API_BASE`, or `GROKPTAH_TOKEN_COMMAND`
cause an earlier ambient-override refusal.

Live execution may be enabled only after an OIDC-only resolver attests all of:

- Grok Build OIDC credential class;
- endpoint `https://cli-chat-proxy.grok.com/v1`;
- the exact requested public `grok-*` model;
- absence of API-key, keychain-key, token-command, and base-URL overrides;
- safe allowlisted refresh issuer/token origins and no-follow, locked private
  credential storage; and
- matching post-call provider observation for route, credential, and model.

Missing authoritative usage or any provider-observer drop makes dependent
claims indeterminate. Observation presence alone never assigns a provider
identity. No live campaign was run while adding this lab.

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
| Reconnect/restart/checkpoint | Declared; unimplemented probes are indeterminate | Owned restart only; the MCP external-Run path does not seed the checkpoint required by `ptah_get_persistent_agent`, so the lab does not fabricate that proof or claim implicit invocation resume. |
| Work | Idempotency/conflict implemented; lifecycle, lease, dependencies, approval declared | Deterministic clock-dependent expiry remains skipped. |
| Routines | Manual activation implemented; schedule/dedupe/lifecycle/recovery declared | Scheduled timing requires deterministic clock support. |
| Coordinator/messages | Parent/child implemented; workers/messages/cursors/expiry/scope declared | Fixed message expiry without clock control is skipped. |
| Native managed execution | Capability present; six probes unimplemented and indeterminate | Public discovery and the nativeExecutor health status are verified, but no generic placeholder can pass a native oracle. |
| Soak | Declared, bounded, not implemented by the minimal runner | Existing coordinator/continuity/soak suites remain complementary evidence. |

On the merged native-executor main, discovery looks only for the audited public tools
`ptah_set_managed_execution`, `ptah_get_managed_execution`,
`ptah_authorize_work_execution`, `ptah_resolve_work_input`, and
`ptah_list_execution_intents`, plus `health.nativeExecutor.enabled=true` from
`ptah_get_capacity`. Even then, an unimplemented probe is indeterminate, not a
pass. Permission checks must use a real elicited permission bound to its exact
Run and are live-only: one fresh Work/Run/permission case is explicitly allowed
and a second independent fresh case is denied. Each pre-execution-gated Work is
authorized through `ptah_authorize_work_execution`; a wrong, dead, or missing
permission resolution must fail closed and leave its Work parked. A single
permission is never treated as having transitioned to both decisions.

The `retryEligible=false` probe observes one native failure and its original
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
