# Persistent Agent certification contract

This document defines the evidence, safety, and bounded-execution contract for
certifying GrokPtah persistent Agents against real model gateways and
deterministic replay fixtures. It is a test contract, not an unattended-runner
implementation and not authorization to broaden Agent permissions.

The machine-readable scenario catalog is
[`evals/persistent-agent-scenarios.v1.json`](../evals/persistent-agent-scenarios.v1.json).

## Purpose and rollout order

Certification begins with the existing native Grok Build OIDC route. That
route exercises GrokPtah's production xAI-compatible client, existing Grok
Build authentication, streaming, tool calls, usage accounting, finite Runs,
checkpoints, and deterministic continuation context.

Provider-neutral support follows after that route is certified. A corporate
OpenAI-compatible gateway uses the same lifecycle assertions, but its default
evidence mode is metadata-only: no prompts, completions, source content,
gateway URLs, account metadata, or response bodies may be captured. Corporate
captures are never promoted into repository fixtures unless the inputs and
outputs are independently synthetic and an authorized human explicitly
approves promotion.

Vercel or other compatible gateways may later become provider adapters to this
same contract. They do not define a separate persistent-Agent lifecycle.

## Product and lifecycle invariants

- An **Agent** is a durable identity intended to remain useful for months or
  years. Its attributable specification revisions, memory memberships, Runs,
  checkpoints, and continuation lineage survive process restarts.
- A **Lane** is a replaceable work context associated with a workspace and
  runtime. Lanes may be created, switched, archived, restored, and eventually
  deleted much more frequently than Agents.
- Archiving a Lane does not retire its Agent, erase Agent memory, invalidate a
  verified checkpoint, or prevent the Agent from continuing in another valid
  Lane.
- A **Run** is one finite, bounded execution. Month- or year-long operation is
  a sequence of finite Runs separated by durable checkpoints and explicit or
  policy-governed continuation. It is never one unbounded provider request or
  one immortal process.
- Every continued Run identifies its Agent, Agent specification revision,
  target Lane, parent Run, checkpoint, continuation input, rendered context,
  effective bounds, and terminal outcome.
- Active Runs remain governed by the specification revision captured at
  admission. Later revisions govern only later Runs.
- Live certification asserts protocol and durable effects. It never requires
  exact model prose, exact wording, or a particular chain of reasoning.

## Certification layers

1. **Hermetic contract replay** sends synthetic fixtures through the real
   production provider client and a scripted loopback gateway. It verifies
   request envelopes, streaming frames, tool-call ordering, retries,
   compatibility fallbacks, usage accounting, and typed failures without a
   network dependency.
2. **Live native xAI certification** uses the existing Grok Build OIDC route,
   synthetic prompts, disposable workspaces, temporary GrokPtah state, and
   explicit campaign bounds. Raw evidence remains ignored.
3. **Persistent lifecycle certification** proves restart, continuation,
   cross-Lane operation, Lane archival independence, interruption,
   idempotency, scoped memory, frozen specification authority, and token
   ceilings.
4. **Endurance certification** repeats finite Runs and restarts while measuring
   continuity and bounded resource growth. It may run for hours or days, but
   every constituent Run and the campaign itself remains bounded.
5. **Provider-neutral certification** reuses the same contract against
   compatible gateways. Corporate gateways begin in metadata-only mode and
   are subject to the stricter evidence restrictions below.

Passing one layer does not imply that a later layer has passed. A live result
does not become a deterministic regression test until a sanitized fixture has
been reviewed and committed.

## Scenario matrix

Scenario IDs are stable contract identifiers. New versions add scenarios or
publish a new catalog version; they do not silently change the meaning of an
existing ID.

| Stable ID | Capability | Live evidence | Hermetic assertion |
| --- | --- | --- | --- |
| `xai-route-oidc-001` | Native Grok Build OIDC route | The qualified built-in xAI profile reaches the Grok Build route and reports a non-secret route class | Required public client headers are present; authorization and identity values are absent from evidence |
| `sse-stream-001` | SSE streaming | A finite response produces ordered text/tool deltas and authoritative usage | Arbitrary byte fragmentation, split UTF-8, usage-only final frames, and `[DONE]` are handled without duplication |
| `native-tools-001` | Native tools | Grok requests a synthetic tool and consumes its result | Roles, call IDs, arguments, result ordering, and exactly-once tool execution are preserved |
| `retry-transient-001` | Retries and downgrade | A bounded transient failure can recover without exceeding campaign limits | Scripted 429, 5xx, timeout, reset, truncated stream, and optional-field rejection produce bounded retries or typed failure |
| `agent-initial-run-001` | Initial persistent Run | Agent, finite Run, terminal state, and checkpoint become durable | Durable state transitions and evidence ordering match the lifecycle contract |
| `restart-between-runs-001` | Process restart | Agent and verified checkpoint remain resumable after host recreation | Reassembled continuation context is byte-identical after restart |
| `resume-same-lane-001` | Same-Lane continuation | A new finite Run continues from the verified checkpoint | Parent Run, checkpoint, spec revision, context ID/hash, and bounds match |
| `resume-cross-lane-001` | Cross-Lane continuation | The same Agent continues in another associated Lane in the same permitted source workspace | Target Lane is attributable and history remains linked to one Agent without using global UI focus as authority |
| `archive-lane-001` | Lane archival independence | Archiving an old Lane leaves the Agent and another valid Lane usable | Agent identity, memory, Runs, checkpoints, and associations remain intact; archive is not Agent retirement |
| `interrupt-recover-001` | Interruption and recovery | Cancellation or restart marks the finite Run interrupted without silently restarting the model | Explicit resume creates exactly one new Run with valid lineage and no fabricated success |
| `resume-idempotency-001` | Idempotent admission | Duplicate resume submission returns the original admitted Run | One request identity produces one durable Run and one provider execution, including after checkpoint advancement |
| `memory-scopes-001` | Project/private/team memory | Synthetic facts in enabled scopes remain useful over later Runs | Disabled scopes are absent; unreadable scopes degrade explicitly; private/team membership is enforced |
| `spec-revision-001` | Frozen Agent specification | A new revision affects a later Run but not an active one | Provider/model, authority, memory, and bounds remain frozen to each Run's captured revision |
| `token-ceiling-001` | Token ceiling | Authoritative provider usage accumulates across rounds and stops at the configured ceiling | Missing usage fails closed while a ceiling is active; overflow and over-limit usage cannot produce success |
| `endurance-finite-runs-001` | Endurance | Many bounded continuations retain selected synthetic facts and recover after scheduled restarts | Run count, lineage windows, journals, artifacts, disk, memory, and file descriptors remain within declared limits |

## Assertions and oracles

Each scenario declares deterministic checks before execution. Acceptable checks
include:

- HTTP method, public route class, path shape, content type, and an explicit
  non-secret header allowlist;
- ordered SSE and tool-call protocol events;
- typed retry, downgrade, cancellation, interruption, and terminal reasons;
- authoritative input, output, and total token usage when supplied by the
  provider;
- durable Agent, Lane, Run, checkpoint, continuation, and idempotency
  relationships;
- exact hashes and byte identity for persisted deterministic artifacts;
- synthetic workspace outcomes such as expected files, tests, or markers;
- bounded request count, elapsed time, output bytes, artifact bytes, process
  memory, and open file descriptors.

Unacceptable checks include exact prose, style judgments, hidden reasoning,
provider request IDs, wall-clock timestamps, latency equality, or a particular
valid sequence of natural-language tokens. Live behavior may vary while still
satisfying the contract.

## Evidence classes and locations

### Raw ignored campaign artifacts

Raw artifacts belong under:

`evals/runs/persistent-agent-cert/<campaign-id>/`

`evals/runs/` is ignored by Git. A raw campaign may retain synthetic request
bodies, synthetic model responses, event journals, disposable orchestration
state, workspace results, aggregate usage, and resource measurements when its
evidence mode permits them. Raw does not mean unrestricted: the forbidden-data
rules apply before bytes are written.

Raw campaigns have finite retention and disk ceilings. They are development
evidence, not repository fixtures and not a supported backup format.

### Sanitized reviewed fixtures

Versioned xAI fixtures belong under:

`evals/provider-contracts/xai/v1/<scenario-id>/`

A fixture contains only the smallest synthetic request envelope, response or
SSE fragments, usage shape, tool-call structure, retry/downgrade sequence, and
expected durable invariants needed for replay. Dynamic IDs, paths, timestamps,
request IDs, account metadata, and incidental prose are replaced with stable
synthetic placeholders.

No live capture is automatically promoted. Promotion requires sanitizer
success, a manifest of every retained field, review of the actual resulting
diff, and explicit human approval before commit.

## Forbidden data

The following must never be written to raw captures, reports, promotion
manifests, fixtures, test snapshots, logs, or failure messages:

- authorization headers, API keys, refresh/access/ID tokens, cookies, signing
  material, or the contents of Grok Build authentication files;
- OIDC subject, user, team, organization, principal, tenant, or account values;
- full arbitrary request or response headers;
- unfiltered environment variables or process arguments;
- real personal or corporate source code, documents, prompts, completions,
  memory facts, terminal output, clipboard data, or Computer Use captures;
- private/corporate gateway URLs, hostnames, IP addresses, certificates, model
  aliases, routing policy, or internal error bodies;
- real home directories, usernames, workspace paths, repository remotes, or
  machine identifiers;
- provider embeddings, opaque binary payloads, or raw vector values.

Secret scanning is mandatory but not sufficient. Capture producers must use
positive field and header allowlists, synthetic inputs, path replacement, and
bounded payloads before persistence. If safe classification is uncertain, the
field is omitted and the scenario records metadata-only evidence.

## Evidence modes

- `synthetic_payloads` permits bounded synthetic request/response bodies plus
  metadata. It is the default for native Grok Build certification.
- `metadata_only` permits protocol classifications, sizes, counts, hashes of
  already-synthetic artifacts, usage, latency, and typed outcomes. It excludes
  prompts, completions, source content, response bodies, private endpoints,
  and account metadata. It is mandatory by default for corporate gateways.
- `disabled` writes no provider observation artifacts. Durable state checks may
  still run against an isolated test home.

Evidence mode never changes provider behavior or Agent authority.

The existing live parity runner accepts `--observation-out <path>` for a
bounded metadata-only smoke report. The report is opt-in, capped at 512 KiB,
and contains only the structural observations retained by the production
provider seam; it does not retain prompts, completions, URLs, credentials,
source paths, or arbitrary header values.

## Bounded campaign profiles

Every campaign sets all bounds explicitly. A runner must reject zero,
unbounded, internally inconsistent, or unenforceable limits.

| Profile | Per-Run tokens | Campaign tokens | Provider requests | Continuations | Duration | Raw disk | Response bytes/request |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `smoke` | 20,000 | 100,000 | 40 | 4 | 30 minutes | 128 MiB | 8 MiB |
| `standard` | 100,000 | 500,000 | 160 | 16 | 2 hours | 512 MiB | 16 MiB |
| `extended` | 250,000 | 2,000,000 | 800 | 96 | 24 hours | 2 GiB | 32 MiB |

Additional rules:

- Round, prompt-byte, per-request timeout, retry, concurrency, and tool-call
  limits remain active in addition to this table.
- A token ceiling requires authoritative usage. Missing or malformed usage
  fails closed; it is not treated as zero.
- Cost is `unknown` unless the provider authoritatively reports it. If a
  monetary ceiling is required but cost cannot be enforced, the campaign must
  not start.
- A campaign stops before admitting work that would exceed request,
  continuation, duration, token, response-byte, or disk limits.
- Endurance campaigns checkpoint progress so the harness itself can restart,
  but resumption never weakens the original campaign ceilings.

## Capture and fixture promotion contract

Promotion is a deliberate review workflow:

1. Select one completed, bounded, ignored campaign produced from synthetic
   inputs.
2. Verify that the provider route is eligible. The initial fixture namespace
   accepts only the public native xAI/Grok Build route class.
3. Parse through a positive schema allowlist; never copy arbitrary JSON or
   headers wholesale.
4. Remove or replace dynamic IDs, timestamps, paths, request IDs, prose,
   account metadata, and machine-specific values.
5. Reject forbidden keys and values, high-entropy credential candidates,
   private endpoints, and content not proven synthetic.
6. Recompute stable placeholder IDs and hashes from the sanitized data.
7. Emit a promotion manifest containing the source campaign ID, scenario ID,
   sanitizer version, retained-field allowlist, transformations, omissions,
   and deterministic checksums. The manifest contains no raw content.
8. Present the resulting fixture and manifest for human review. Promotion
   requires explicit approval; CI or an Agent may not self-approve it.
9. Replay the reviewed fixture through the production provider client and run
   secret/privacy checks before commit.

Corporate metadata-only captures are not eligible for promotion by default.
An exception requires independently synthetic input and output, removal of all
corporate routing metadata, and explicit approval by a person authorized to
release that material.

## Result states and reporting

Each scenario ends as `passed`, `failed`, `skipped`, or `inconclusive`.
Missing credentials, unavailable live service, or absent authoritative usage
is not a pass. Reports distinguish:

- protocol conformance;
- durable lifecycle conformance;
- safety and redaction conformance;
- bound enforcement;
- workspace oracle results;
- resource-growth observations;
- skipped or inconclusive evidence and its reason.

A campaign report identifies the repository commit, dirty-state flag, scenario
catalog version, public provider route class, model ID, evidence mode, bound
profile, actual usage, terminal reasons, and deterministic check results. It
does not contain credentials, private route details, raw prompts/completions,
or exact-prose expectations.

## Definition of certified

A provider/runtime combination is certified for a named catalog version and
campaign profile only when all required scenarios for that layer pass, every
bound is enforced, forbidden-data scans pass, restart evidence is durable, and
the report is reproducible from reviewed fixtures where a hermetic equivalent
exists. Certification is evidence for that version and profile; it is not a
promise of unlimited operation or authority.
