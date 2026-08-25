# Enterprise gateway campaign evidence

This contract makes Stage 4 / independent enterprise-review requirements
explicit. It does **not** run a live company gateway, spend quota, or exercise
a Cursor account. A green Desktop job, an offline fixture, and a hand-labeled
`live_campaign` field are not live proof and do not qualify a release.

Schema: `grokptah.enterprise-gateway-campaign.v1`

Implementation: `crates/codegen/grokptah-agent-bridge/src/enterprise_gateway_campaign.rs`

## What this slice proves (offline)

The deterministic verifier and loopback fake gateway prove that:

1. **Restricted company gateway identity is recorded.** The bundle carries a
   `request_id` plus requested profile, base URL, model, tenant label, provider
   kind, and class. Silent fallback onto the built-in frontier family
   (`xai` / `api.x.ai`) or drift of base URL, tenant, provider kind, profile,
   or model fails closed.
2. **Quota/usage truth is an explicit provider receipt bound to that route.**
   The quota object must repeat `request_id`, base URL, tenant, provider kind,
   profile, and model. Unknown quota, contradictory arithmetic
   (`used + remaining != limit`), local session inference
   (`source != provider`), route drift, or a hand-labeled `live_campaign`
   quota fail closed. An offline fixture receipt is labeled
   `offline_fixture` and is not a live quota claim.
3. **Retries are idempotent and auditable.** Payloads are canonicalized before
   hashing (JSON object key order/whitespace does not create a new request).
   Attempts are contiguous, carry a SHA-256 payload hash, and identical
   `request_id` + canonical payload hashes replay. Replayed success and error
   [`ErrorEnvelope`](../crates/common/grokptah-agent-sdk/src/error.rs) values
   must match the original attempt. Payload drift is a bounded
   `invalid_request`. Pending and uncertain outcomes are represented honestly
   and fail closed until reconciled. This campaign does not write the
   external-worker ledger.
4. **Release/promotion refuses to qualify from schema labels.**
   `qualified_for_release` stays false unless this verifier independently
   evidences live restricted-gateway, live provider quota, live Cursor-account,
   live HTTPS retry/idempotency, and release-artifact gates. It never contacts
   those systems, so `remaining_live_gates` always lists all five. Loopback
   URLs, offline fixtures, and hand-labeled `live_campaign` fields cannot
   clear them. A missing unsigned Desktop Release Build artifact is not a
   substitute.
5. **Weak or unavailable providers return honest bounded errors.** Public
   `ErrorEnvelope` text is needle-free: no `api_key`, `authorization`,
   `bearer`, `credential`, `credential_ref`, provider URLs, or `[redacted]`
   placeholders. Bounded provider errors may be redacted, but privileged
   diagnostics are currently discarded rather than exposed through an
   operator channel. The loopback HTTP harness preserves the public error
   taxonomy (`invalid_request` vs provider-unavailable) without privileged
   text; pending/uncertain retries stay HTTP-status stable and keep the
   uncertain envelope. `collect_offline_campaign` retains failed and
   replayed unavailable receipts from `probe` so `verify_campaign` can
   prove fail-closed behavior. It does not invent a `replayed` outcome
   from a discarded error.

## What this slice does not prove (still required)

The following remain **open live gates**. The verifier lists them on every
verdict as `remaining_live_gates`:

- a live restricted-company gateway campaign (fixed route, tenant, model,
  authorization boundary, no frontier fallback);
- a live provider quota/usage receipt from that gateway, bound to the same
  `request_id` and route identity;
- a live Cursor-account campaign: a secret-free receipt whose API base is
  exactly `CURSOR_CLOUD_API_BASE` (`https://api.cursor.com`) plus a stable
  run/campaign identifier. A company-gateway URL is never a Cursor-account
  receipt; provider, base URL, or tenant drift fails closed;
- live HTTPS retry/idempotency evidence on those same routes, including
  matching replayed success/error envelopes;
- an unsigned or signed release artifact produced by the reviewed head.

Do not treat GitHub Desktop CI, this fake gateway, the Cursor Cloud fake API
in `external_worker/cursor.rs`, local `/usage` counters, or a bundle that
stamps `live_campaign` on schema fields as those receipts.

The protected Stage 6 always-on soak is a separate loopback-provider
continuity campaign. This slice must not retarget, undraft, or interrupt it.

## Authority boundaries reused

- Provider profiles and `ProviderKind` from `gateway_config`
  ([Provider profiles](./PROVIDER_PROFILES.md)).
- Share-safe `ErrorEnvelope` / `ErrorCode` from `grokptah-agent-sdk`.
- Cursor family identity `ExternalWorkerProvider::CursorCloud` and
  `CURSOR_CLOUD_API_BASE` from
  [Cursor Cloud integration](./CURSOR_CLOUD_INTEGRATION.md). Streaming,
  merge/undraft, and live artifact listings without `runId` stay unsupported
  there.
- Idempotency shape matches the external-worker ledger: `request_id` plus
  canonical payload hash, replay vs payload drift, Pending/Uncertain fail
  closed. This campaign does not write that ledger.

## Offline vs live kinds

| `EvidenceKind` | Meaning |
| --- | --- |
| `absent` | No receipt. Quota and release gates fail closed. |
| `offline_fixture` | Loopback or scripted provider. Contract checks only. |
| `live_campaign` | Operator assertion of a named live run. This verifier rejects it as evidence: labels cannot qualify a release, and `remaining_live_gates` stay populated. A separate live campaign must still attach secret-free receipts that a live verifier can independently check. |

## How to run

From `crates/codegen/grokptah-agent-bridge`, with an isolated target so the
protected soak is untouched:

```sh
CARGO_TARGET_DIR=/tmp/grokptah-enterprise-gateway-target CARGO_INCREMENTAL=0 \
  cargo test --locked --lib enterprise_gateway_campaign -- --test-threads=1
CARGO_TARGET_DIR=/tmp/grokptah-enterprise-gateway-target CARGO_INCREMENTAL=0 \
  cargo test --locked --test enterprise_gateway_campaign -- --test-threads=1
```

`schema_fixture_with_complete_live_fields_does_not_qualify` is a **negative**
path: a complete live-shaped bundle still has `qualified_for_release=false`
and a full `remaining_live_gates` list. It is not a live company-gateway
campaign.
