# Live provider continuation

Synthetic campaigns set `providerCalls: 0` and `eligibility: synthetic_only`.

A later live lane MUST:

1. Reuse `grokptah.cu_eval_scenario.v1`, `grokptah.cu_eval_episode_result.v1`,
   `grokptah.cu_eval_evidence.v1`, and `grokptah.cu_eval_campaign_report.v1`
   without weakening additionalProperties / deny-unknown-fields rules.
2. Keep the same closed action grammar and fail-closed parsing.
3. Set `eligibility` to `live_reusable_schema` only when `providerCalls > 0`.
4. Set `live_authoritative` only with a structured `ProviderReceipt`
   (`receiptId`, `providerId`, `modelId`, `configDigest`, `contentSha256`).
   A caller Boolean is not authority. Synthetic verification and live
   verification are separate entry points; a synthetic campaign with
   `providerCalls != 0` or live receipts is rejected by the synthetic verifier.
5. Leave `costUsd` null unless the provider receipt contains a cost figure.
6. Record `modelInputUnitsKind: provider_tokenizer_tokens` only for real
   tokenizer counts. Fake adapters may not use that kind.
7. Never treat a synthetic PASS as live cheap-gateway or frontier qualification.
8. Keep credentials out of reports. Disposable simulator/demo data only (#272).

This crate refuses `GROKPTAH_CU_ADAPTIVE_LIVE=1` rather than pretending to call
a provider.
