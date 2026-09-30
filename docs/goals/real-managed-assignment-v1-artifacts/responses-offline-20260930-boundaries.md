# Offline production boundary evidence

Functional revision: `b21b7f4d1e3d139e4b40ca8d9f6b38e1c5622c73`.

The meaningful Responses red run reached the real managed Work/Run/Attempt and relay with the new Responses scaffold but without canonical `xai_responses` wire admission. It reserved one request and failed before wire: local fixture sends **0**, canonical attempts **0**, `before_wire_rejected`, no candidate, no passed checks, source unchanged. Work `3e008205-6b9d-40be-a4d5-5e8279127ffb`, Run `b124cbf5-6df1-41cc-91b8-36934053fb41`, Attempt `a7cde8a1-04e4-406d-89ba-2a90c7ea12fb`. This is an admission failure, not a provider receipt or a replay of the historical live attempt. Preliminary native-sandbox launch denials and an incomplete synthetic `response.created` envelope were fixture setup failures, not this functional red result.

The green revision admits the sealed dialect through the existing canonical send authority. Case C reaches operator prepare, one Work/Run/Attempt, relay, canonical send, local Responses, actual pinned-CLI file tools, a retained two-file candidate, host oracle and review. Source oracle fails first. The candidate corrects UTF-8 byte length and strict exact-frame decoding, passes the host oracle, and remains unapplied. The second request contains encrypted reasoning, two function calls and their two results. Its sealed request is 14,645 bytes; admission reserves settled 115 + bytes 14,645 + owned prior output 15 + sealed output 1,024 = **15,799 <= 16,000**. All three credential-free reopens retain the same provider evidence and cause zero extra fixture sends. Full identities, request shapes, candidate diff and oracle digests are in the journey artifact.

| Mode | Fixture sends | Host total / reasoning subset | Cost ticks | Outcome |
| --- | ---: | --- | --- | --- |
| A: ordinary 100/10/0/110 each round | 2 | 220 / 0 | UNKNOWN | Green retained candidate |
| B: reasoning 100/15/5/115 each round | 2 | 230 / 10 | 1554 | Green retained candidate |
| C: cached 20 within input 100 each round | 2 | 230 / 10; cached 40 | 1554 | Green retained candidate |
| D: first output 15, final output exactly 1024 | 2 | 1239 / 105 | known 0 | Green retained candidate |
| E: incomplete at output 1024 | 1 | 1124 / 100 | known 0 | Known bounded settlement, terminal revocation, no successful harvest |
| F: contradictory total 116 versus 115 | 1 | UNKNOWN | UNKNOWN | `usage_inconsistent`, revoked/uncertain |
| G: reasoning 16 exceeds output 15 | 1 | UNKNOWN | UNKNOWN | `usage_inconsistent`, revoked/uncertain |
| H: unknown usage extension | 1 | UNKNOWN | UNKNOWN | `usage_inconsistent`, revoked/uncertain |
| Missing usage | 1 | UNKNOWN | UNKNOWN | `usage_missing`, revoked/uncertain |
| Malformed stream | 1 | UNKNOWN | UNKNOWN | `protocol_failure`, revoked/uncertain |

The direct pinned-binary A–H projection campaign is separate from these integrated two-round cases: A/B/D/E/F/G/H used one intended request; C used two and executed both file writes. CLI F recomputes a contradictory total and CLI G retains an invalid reasoning count. Neither client behavior authorizes host settlement. The relay validates the complete bounded response before forwarding it. Host evidence never treats rejected-call aggregate zero counters as known zero usage.

## Adversarial coverage on the production relay

The eight `responses_*` tests in `managed_responses.rs` exercise cap omission/raise, legacy aliases, wrong model/backend/path, server tool substitution, reasoning configuration disagreement, remote/image input, malformed/repeated stream terminals and function-call mismatches, output above cap, missing/unknown/contradictory receipts, reasoning above output, integer overflow, cache creating no authority, repeated-turn reserve depletion, and foreign/modified/repeated opaque reasoning. The real relay opaque-context test deliberately removes only the additional 15-token reuse allowance from remaining authority and proves zero second sends.

Existing completed-forward handoff and abandonment tests run both dialects: dropping a completed predecessor cannot clear a successor's in-flight state, poison its diagnostic or restore its authority; cancellation/drain/changed-request abandonment is terminal. Restart behavior is also exercised by the integrated three-reopen journeys. Existing official model-header, safe diagnostics, source isolation and Verified Change tests remain required in the full bridge suite. Legacy additive Chat receipts still fail closed; only existing disposable legacy regression candidates undergo explicit fixture approval/apply.

No additional live provider request, new Responses source application, historical attempt retry, merge, release or deployment occurred.
