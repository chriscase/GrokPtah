/goal Replace the managed Grok Build qualification path's ambiguous
Chat-Completions output bound with a reasoning-inclusive Responses-API
bound, if and only if the exact pinned CLI and production architecture
support that route safely.

This assignment is OFFLINE ONLY.

Continue:
Repository: chriscase/GrokPtah
Branch: grok/real-managed-assignment-v1
PR: #580

Current published tip:
81cddffd9e36ac2da3b61dfb2706471bc4a11aeb

Published tree:
a3affa014cb4407e0a5c6aa6f51389c264b8e8a0

Accepted executable revision:
6c91c00b49d97555a5e6e5de8eaf05fb90066e3f

Functional tree:
8ecc48bc0a74714f9fcee7e6ea945f5c3fee0802

Expected main:
9e2660ff1a50edb7af1b81a4232d7fe23a37de0d

Pinned CLI:
Grok 1.0.41
revision label 4220f3b224a6
SHA-256:
9c844eb13365180787d9ad22b2b3748a024be8e1ed845253cc114781b31c591d

The latest live observation is independently accepted as evidence:

prompt_tokens = 3591
completion_tokens = 11
reasoning_tokens = 260
total_tokens = 3862
cached_tokens = 128
cost_in_usd_ticks = 40306000

Observed relationship:

3591 + 11 + 260 = 3862

Do not alter or reinterpret that historical Work.

The current Chat-Completions managed route remains unsupported for additive
reasoning because the host has not established that its legacy max_tokens
field provides the required pre-send cap over visible completion plus
reasoning.

Do NOT make another provider request merely to probe that legacy ambiguity.

MISSION

Determine whether the exact Grok CLI 1.0.41 can run the same bounded
grok-build-0.1 agent workflow through the Responses API using a
reasoning-inclusive max_output_tokens bound.

If it can, implement and fully qualify that route OFFLINE.

If it cannot, return the exact blocker instead of broadening the old
Chat-Completions contract.

AUTHORIZATION

Authorized:
- Exact pinned-CLI execution against localhost fixtures.
- Synthetic OIDC credentials.
- External-network-denied native sandboxing.
- Production implementation needed for a Responses-backed managed route.
- Tests, local validation, hosted CI and publication to existing PR #580.
- Public documentation/source research.

Not authorized:
- Any live xAI inference request.
- Login, refresh, entitlement, account, billing or model-list calls.
- Any retry or mutation of historical live Work/Run/Attempts.
- Relaxing the 16,000-token assignment ceiling.
- Increasing the 1,024 generated-token per-request ceiling.
- Source application.
- Merge, release or deployment.
- A generic provider abstraction rewrite.

1. ESTABLISH THE RESPONSES CONTRACT

Use current primary xAI documentation and pinned-client behavior.

Establish and cite the applicable contract for:

- POST /v1/responses
- max_output_tokens
- input_tokens
- output_tokens
- output_tokens_details.reasoning_tokens
- total_tokens
- cached input
- function/tool calls
- incomplete responses caused by max_output_tokens

The safety property required by this goal is:

    max_output_tokens <= 1024

must be a server-enforced pre-send upper bound over all generated model
tokens charged against the assignment's output budget, including reasoning.

Do not treat reasoning_effort as a numerical cap.

Do not use timeout, response-byte size, post-response validation or observed
historical behavior as substitutes for a pre-send token bound.

Record exactly what source establishes the bound and its applicability.

2. PROVE EXACT CLI 1.0.41 RESPONSES SUPPORT OFFLINE

Run the SHA-verified installed CLI under a disposable HOME/GROK_HOME and
deny-by-default macOS sandbox with:

- synthetic OIDC credential;
- localhost-only network;
- no operator files;
- no real credential;
- no update/telemetry/plugin/MCP/web/background traffic;
- the existing synthetic two-file UTF-8 repair fixture.

Configure the intended grok-build-0.1 model to use the Responses backend.

Do not rely on current-main Grok Build source as proof that CLI 1.0.41
behaves the same way. Capture the exact binary.

The loopback fixture must capture the intended model request and prove:

- endpoint is POST /v1/responses;
- model is grok-build-0.1;
- max_output_tokens is present and exactly the host-selected value;
- the child cannot raise or remove that value;
- tools are the intended client-side file tools only;
- no unwanted server-side tool authority is granted;
- reasoning effort and other relevant parameters are known;
- the expected OIDC/model-routing headers are present;
- no auxiliary request can consume managed-assignment authority.

Exercise a complete tool round trip:
model -> file tool call -> tool result -> model.

Use synthetic responses with nonzero reasoning.

3. CHARACTERIZE RESPONSES USAGE THROUGH PINNED CLI

Use several synthetic provider receipts.

At minimum:

A. Ordinary:
input=100
output=10
reasoning=0
total=110

B. Reasoning:
input=100
output=15
reasoning=5
total=115

where reasoning is a subset of output.

C. Cached:
input=100
cached=20
output=15
reasoning=5
total=115

D. Exactly-at-cap output.

E. Provider returns an incomplete/max-output condition.

F. Contradictory totals.

G. reasoning > output.

H. unknown usage/cost extension.

Capture:
- CLI headless projection;
- durable CLI usage/journal projection;
- host accounting;
- candidate/tool behavior.

Do not assume the existing Chat-Completions projection rules carry over.

4. IMPLEMENT THE MANAGED RESPONSES ROUTE ONLY IF PROVED

If the exact pinned CLI works correctly through Responses, extend the existing
ManagedProviderRelay narrowly.

Do not add a general second provider architecture.

The managed route should expose only the production surfaces required by the
pinned child, including /v1/responses as necessary.

The host must seal:
- official OIDC CLI-proxy destination;
- grok-build-0.1;
- Responses backend;
- max_output_tokens = assignment-selected value, never greater than 1024;
- allowed tools;
- reasoning configuration;
- request/response byte bounds;
- assignment identity.

The child must not be able to:
- remove max_output_tokens;
- increase it;
- switch backend;
- switch model;
- introduce server-side provider tools;
- change the upstream route.

Reject disagreement rather than silently honoring child authority.

5. ACCOUNTING MODEL

For Responses usage, keep these concepts explicit:

Provider input:
    input_tokens

Provider generated output:
    output_tokens

Reasoning subset:
    output_tokens_details.reasoning_tokens

Provider total:
    input_tokens + output_tokens

Cache:
    subset of input_tokens

Never add reasoning twice.

For the synthetic example:

input = 100
output = 15
reasoning = 5
total = 115

host chargeable token total is 115, not 120.

The 5 reasoning tokens remain separately visible for audit while already
being contained in output=15.

Use checked arithmetic.

Unknown or contradictory forms fail closed without restoring request
authority.

Preserve provider-reported cost ticks separately. Missing cost remains
UNKNOWN, never zero.

6. PRE-SEND BUDGET GUARANTEE

This is the critical acceptance property.

Before each physical request:

remaining assignment token authority
must cover:

    conservative input reservation
    + sealed max_output_tokens

Because the Responses contract bounds reasoning inside max_output_tokens,
no successful or truncated response may incur more generated-token authority
than was reserved.

Prove with adversarial tests that:
- max_output_tokens cannot exceed 1024;
- reasoning cannot escape that reservation;
- incomplete-at-cap responses remain accountably bounded;
- repeated turns consume settled totals before another reservation;
- unknown/incomplete accounting prevents another send;
- integer overflow fails closed;
- cache never creates budget;
- a child cannot substitute max_completion_tokens/max_tokens to bypass the
  host-owned Responses bound.

Keep aggregate assignment ceiling = 16000.

Do not raise it.

7. PRESERVE CHAT COMPLETIONS SAFELY

Do not delete the existing Chat-Completions implementation if other code
depends on it.

But for this managed Grok Build production profile:

- do not treat additive-reasoning Chat-Completions responses as supported;
- do not weaken its existing fail-closed accounting;
- do not assert legacy max_tokens is a joint reasoning bound without contract
  evidence.

The new managed qualification route may select Responses instead.

Existing historical Chat-Completions receipts remain unchanged.

8. PRODUCTION-PATH OFFLINE RED-TO-GREEN JOURNEY

Once the route is implemented, run the actual pinned CLI through:

operator prepare
-> Work
-> Run
-> Attempt
-> managed relay
-> canonical send authority
-> local Responses fixture
-> file-tool execution
-> retained candidate
-> host-owned oracle
-> review

Use the existing two-file UTF-8 task.

Require:
- red source oracle;
- meaningful two-file candidate;
- green retained-candidate oracle;
- exact Work/Run/Attempt;
- complete accounting with nonzero reasoning;
- max_output_tokens <= 1024 on every fixture send;
- source unchanged before explicit disposition;
- three reopens with no additional fixture sends.

This remains OFFLINE qualification because the upstream is local.

Do not apply the candidate unless an existing disposable-source regression
requires explicit apply as part of the already-accepted workflow test.

9. MEANINGFUL NEGATIVE COVERAGE

At minimum prove:

- Child omits max_output_tokens -> host refuses or seals safely.
- Child asks for >1024 -> no send.
- Wrong model -> no send.
- Wrong backend/path -> no send.
- Server-side tool introduced -> no send.
- reasoning > output -> terminal rejection.
- input + output != total -> terminal rejection.
- response output exceeds sealed bound -> terminal uncertainty/revocation.
- missing usage -> no second request.
- malformed stream -> no second request.
- abandoned request -> capability terminal.
- completed predecessor cannot alter successor state.
- restart cannot duplicate a physical request.

Preserve all previously accepted abandonment, successor ownership, model-header,
diagnostics, source isolation and Verified Change regressions.

10. IF RESPONSES CANNOT BE USED

Do not force it.

Return:

RESPONSES ROUTE NOT SUPPORTABLE — EXTERNAL CONTRACT REQUIRED

and identify exactly which condition fails:

- pinned CLI cannot use Responses for grok-build-0.1;
- required file tools cannot round-trip;
- max_output_tokens is not present/sealable;
- CLI proxy route incompatibility remains;
- accounting projection cannot be reconciled;
- or another concrete blocker.

Prepare the smallest precise xAI support question.

Do not fall back to accepting ambiguous Chat-Completions max_tokens.

11. VALIDATION

If executable code changes, run:

- focused Responses relay and accounting tests;
- exact pinned-CLI localhost qualification;
- full locked bridge suite with fresh GROKPTAH_HOME;
- bridge fmt and strict all-target Clippy;
- service check/fmt/Clippy/tests;
- host-authority tests;
- frontend typecheck/tests;
- Tauri tests;
- existing Chat-Completions offline regressions;
- existing Verified Change suite;
- exact-functional Hosted Desktop CI.

No live provider request is authorized.

12. PUBLICATION

Continue the same branch and PR #580.

Report:
- starting SHA/tree;
- final functional SHA/tree;
- final evidence SHA/tree;
- exact executable diff;
- pinned CLI Responses capture;
- authoritative max_output_tokens contract evidence;
- host accounting mapping;
- before/after tests;
- complete offline managed journey;
- validation counts;
- exact-functional hosted run;
- historical preservation;
- additional live provider requests: 0.

Return one:

RESPONSES MANAGED ROUTE READY FOR INDEPENDENT REVIEW

RESPONSES ROUTE NOT SUPPORTABLE — EXTERNAL CONTRACT REQUIRED

BLOCKED — with exact reason

Then stop.

Do not make a live request, merge, release, deploy, or begin another goal.