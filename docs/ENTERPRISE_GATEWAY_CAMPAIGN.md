# Enterprise gateway campaign evidence

This contract makes Stage 4 / independent enterprise-review requirements
explicit. It does **not** run a live company gateway, spend quota, or exercise
a Cursor account. A green Desktop job and an offline fixture are not live
proof and do not qualify a release.

Schema: `grokptah.enterprise-gateway-campaign.v1`

Implementation: `crates/codegen/grokptah-agent-bridge/src/enterprise_gateway_campaign.rs`

## What this slice proves (offline)

The deterministic verifier and loopback fake gateway prove that:

1. **Restricted company gateway identity is recorded.** The requested profile,
   base URL, model, tenant label, and class are part of the bundle. A silent
   change onto the built-in frontier family (`xai` / `api.x.ai`) fails closed.
2. **Quota/usage truth is an explicit provider receipt.** Unknown quota,
   contradictory arithmetic (`used + remaining != limit`), or local session
   inference (`source != provider`) fail closed. An offline fixture receipt is
   labeled `offline_fixture` and is not a live quota claim.
3. **Retries are idempotent and auditable.** Attempts are contiguous, carry a
   SHA-256 payload hash, and identical `request_id` + payload hashes replay.
   Payload drift is a bounded `invalid_request`. Failed attempts keep an
   [`ErrorEnvelope`](../crates/common/grokptah-agent-sdk/src/error.rs).
4. **Release/promotion refuses to qualify when live evidence is absent.**
   `qualified_for_release` stays false unless live restricted-gateway, live
   provider quota, and live Cursor-account fields are all present on a
   non-loopback HTTPS company route. Loopback URLs cannot be advertised as
   `live_campaign`. A missing unsigned Desktop Release Build artifact is not a
   substitute for those receipts.
5. **Weak or unavailable providers return honest bounded errors.** Public
   errors reuse `ErrorEnvelope` (`authority_unavailable`, `capacity`,
   `invalid_request`). Bearer tokens, `api_key` assignments, credential
   references, and provider URLs are redacted. No campaign field stores an
   API key or keychain reference.

## What this slice does not prove (still required)

The following remain **open live gates**. The verifier lists them on every
offline fixture verdict as `remaining_live_gates`:

- a live restricted-company gateway campaign (fixed route, tenant, model,
  authorization boundary, no frontier fallback);
- a live provider quota/usage receipt from that gateway;
- a live Cursor-account campaign (disposable Cloud Agent against
  `https://api.cursor.com`, no credentials in evidence);
- live retry evidence on those same routes;
- an unsigned or signed release artifact produced by the reviewed head.

Do not treat GitHub Desktop CI, this fake gateway, the Cursor Cloud fake API
in `external_worker/cursor.rs`, or local `/usage` counters as those receipts.

The protected Stage 6 always-on soak is a separate loopback-provider
continuity campaign. This slice must not retarget, undraft, or interrupt it.

## Authority boundaries reused

- Provider profiles and `ProviderKind` from `gateway_config`
  ([Provider profiles](./PROVIDER_PROFILES.md)).
- Share-safe `ErrorEnvelope` / `ErrorCode` from `grokptah-agent-sdk`.
- Cursor family identity `ExternalWorkerProvider::CursorCloud` from
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
| `live_campaign` | Operator assertion of a named live run. Rejected on loopback/frontier URLs. The verifier does not contact the network; a separate live campaign must still attach secret-free receipts. |

## How to run

From `crates/codegen/grokptah-agent-bridge`, with an isolated target so the
protected soak is untouched:

```sh
CARGO_TARGET_DIR=/tmp/grokptah-enterprise-gateway-target CARGO_INCREMENTAL=0 \
  cargo test --locked --lib enterprise_gateway_campaign -- --test-threads=1
CARGO_TARGET_DIR=/tmp/grokptah-enterprise-gateway-target CARGO_INCREMENTAL=0 \
  cargo test --locked --test enterprise_gateway_campaign -- --test-threads=1
```

`schema_fixture_with_complete_live_fields_would_qualify` is a **schema-only**
positive path. It is not a live company-gateway campaign.
