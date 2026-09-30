# Real Managed Assignment v1 — next-live proposal (NOT EXECUTED)

This document grants no provider authority. No new live repository, Work, attempt, request ID, entitlement probe or inference request was created for it. The historical failed Work remains cancelled: one child attempt, one recorded live admission/send, unknown/incomplete usage, no candidate and no application. The original frozen G1–G10 goal remains PARTIAL.

## Exact proposed contract

- Native macOS Grok CLI 1.0.41, SHA-256 `9c844eb13365180787d9ad22b2b3748a024be8e1ed845253cc114781b31c591d`; existing inspect-json-v1 contract and private child home.
- Child custom-model alias `managed-assignment`, wire model `grok-build-0.1`, Chat Completions SSE through the production host relay and canonical provider-send authority.
- Proposed upstream route: `https://cli-chat-proxy.grok.com/v1/chat/completions`, authentication class: host-held Grok Build OIDC user session. No API-key route or authentication-class fallback is authorized by this proposal. Class/route disagreement is a prewire stop.
- Parent applies the existing CLI proxy headers: CLI version 1.0.41, token-auth product `xai-grok-cli`, authenticate-response mode and the existing host scope. Header values carrying credentials or user/team identities are never evidence. The child has only its expiring relay capability; no upstream credential.
- File tools and the current disabled command/plugin/MCP/web/compatibility surfaces remain unchanged. Mutation is restricted to the two allowed paths in a private clone with separate Git control, no remotes and no operator-home/keychain/external network authority.

## Qualification source and host oracle

Create a NEW disposable repository only after separate owner authorization. Copy the exact two red source files under `evals/fixtures/real-managed-assignment-v1/source` from the independently accepted repaired functional revision; preserve source file modes 100644. Expected source tree: `1ef9c0872b4eadfaae5f75bbf2933c062cce67ec`.

A reproducible proposed root commit is `22449e29564be2ff9b9fe7a7f92e6525a3193416`: that tree, no parent; author and committer `GrokPtah qualification <qualification@grokptah.invalid>`, both timestamp 946684800 +0000; message `Real managed assignment v1 qualification base` followed by one newline. Its identity was computed without creating a qualification repository or writing a Git object. Freeze and verify the actual base before readiness.

Allowed files are exactly `src/framing.py` and `src/decoder.py`. The host registers an external oracle from the same functional revision: `oracle/check.py` SHA-256 `ea6453adb05e49fdf427e304565f024f816569e4bbccf85b095816bf056a0032`; `oracle/check.sh` SHA-256 `b04ed3bb317eaa1a038939409c39a005253fcb230fbd86feb65a7d9895c668c0`. Freeze its actual directory/executable/profile digests and revision before admission. Use private check confinement revision 2, no network, root-owned public Python/Xcode, 5-second timeout, 16-KiB captured-output and 64-KiB output-directory ceilings.

The oracle must fail on the red base, then pass on the exact retained two-file candidate for UTF-8 byte-length encoding, canonical unsigned framing, exact payload length and strict UTF-8 decoding. A worker verdict alone cannot verify it. Source stays untouched through review; approval/application require a separate explicit disposition bound to the inspected candidate, diff bundle, check profile and source revision. No source application is included in the proposed live authorization.

## Finite authorization proposal

| Bound | Proposed ceiling (unchanged) |
| --- | ---: |
| Worker attempts | 1 |
| Lease/worker lifetime | 180,000 ms |
| Physical admissions/model rounds | 6 |
| Aggregate reported input plus output tokens | 16,000 |
| Output tokens per request | 1,024 |
| Prompt bytes | 16,000 |
| Request body | 64 KiB |
| Upstream response | 256 KiB |
| Returned tool calls across the lease | 12 |
| Automatic retry / rearm / paid diagnostic probe | 0 |

The existing conservative byte-sized prompt reservation applies before each admission. Unknown usage is never a zero-cost refund. Any uncertain/abandoned/unsettled forward terminates the entire capability for every request body. Revocation does not undo a remote request; a numeric HTTP rejection does not establish zero billing. Token ceilings are not a guaranteed dollar charge or a promise of complete usage after failure.

## Readiness and accounting gates before any paid dispatch

Verify exact CLI digest/version/inspect, clean exact source/base/tree, private empty worker root, registered sealed external oracle, approved one-attempt Work scope, host-controlled verification, expected authentication class/route, credential freshness and relay lifetime/bounds. Readiness records zero provider admissions and does not establish entitlement. No credential-bearing completion or entitlement probe may be used as readiness.

Supported stream evidence currently requires one choice, completed `stop`/`tool_calls`, one complete usage event, one DONE marker, consistent prompt/completion/total tokens, bounded tools and no secret echo. Headless model-round counters must match settled host calls; session usage must contain exactly one user prompt and reconcile both aggregate and prompt counters with those calls/tokens. Missing or inconsistent usage is a terminal safe failure.

**Accounting compatibility remains a gate.** The public xAI Chat Completions example includes reasoning in total tokens beyond prompt plus completion. Grok headless documentation separates cached from uncached input and can add complete/partial cost fields. Those nonzero reasoning/cache/additional-cost forms have not been qualified by this pass's zero-cache/zero-reasoning loopback fixtures. The parser was not broadened for them. Before a new paid run, establish a supported complete contract through primary-source research and offline implementation/validation if required; do not spend a diagnostic request to discover it and do not silently discard those fields. This proposal is not an entitlement or general live-compatibility claim.

Primary references: [coding model](https://docs.x.ai/developers/models/grok-build-0.1), [Chat Completions usage contract](https://docs.x.ai/developers/rest-api-reference/inference/chat-completions), and the pinned repository's `xai-grok-pager/docs/user-guide/14-headless-mode.md`. Current public docs do not identify the historical failed request's cause.

## Safe evidence on every outcome

Persist bounded typed evidence on the existing managed intent/Work–Run projections: reserved/admitted counts, observed HTTP response count/status, completed usage, local accounting completeness, remote uncertainty (unknown remains unknown on lost-relay/legacy recovery), revocation, first interruption and at most 32 diagnostic records. Retain only validated bounded request identifiers and allowlisted machine error enums; never upstream prose, bodies, prompts, arbitrary headers, bearer/cookie values or sensitive URLs.

Prewire denials preserve their typed admission reason; admitted transport failure remains uncertain; HTTP 400/401/403/429/503 preserves numeric status and any safe code; SSE/usage/settlement failure remains terminal; child exit/metadata mismatch retains existing safe classification refs; cancellation, expiry, abandonment and supervision recovery remain explicit. Every failure ends this authorization, retains unknown usage, enters durable review/discard and permits no new attempt after reopening. Verify stable Work/Run/Attempt counts and source fingerprints on repeated reopen.

## Owner decision still required

Independent acceptance of the repaired functional revision is NOT claimed. After that review and accounting readiness, the owner must explicitly authorize exactly one NEW provider-backed worker attempt on the above NEW disposable red repository, at the pinned CLI/model/OIDC route and finite ceilings, with no retry, no paid probe and no source application. That authorization must identify the accepted functional SHA, actual frozen repository/base, retained-evidence scope and the possible unknown charge after interruption. Historical Work cannot be reused or reset. Nothing in this proposal authorizes execution now.
