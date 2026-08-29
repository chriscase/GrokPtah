# Synthetic campaign summary (seed 435272, repeats 5)

Status: **PASS** for the exact source identity below. A later documentation-only
child is not silently substituted for this evaluated code head.

Exact evaluated code head: `be38197ba05bd5b572d7cf5cbd94e51955d70e86`

Exact evaluated tree: `f1ca0c62004c92c926c357ef7b43f8b5bc7eb75e`

Base source gate: `c6f1cb23e9d6217005599850d9e0d6f7df64d5a1`

| Metric | Value |
| --- | --- |
| Episodes | 2100 |
| Task success | 425 / 425 |
| Unauthorized dispatches | 0 |
| Safety violations | 0 |
| Provider calls | 0 |
| Cost USD | null |
| Abstentions | 135 |
| Escalations | 205 |
| Invalid actions | 3020 |
| Stale-action attempts | 330 |
| Postcondition failures | 0 |
| Recovery converged | 60 / 60 |
| Economy image bytes | 0 |
| High Assurance image bytes | 12920 |
| Economy observation bytes | 657325 |
| High Assurance observation bytes | 846565 |
| Held-out | heldout.card2_ok |
| Fixture hash | 614a8b4b0bf5d5f559764f894661475a11e75e1e40279bdbe5e48cf5387cc20a |
| Campaign digest | a6b24cd62d30f1ba21393667faf4020133d1e08e77c05bcea61ad16591390d1e |

Regenerate:

```sh
REPO="$(git rev-parse --show-toplevel)"
EXPECTED_HEAD=be38197ba05bd5b572d7cf5cbd94e51955d70e86
test "$(git -C "$REPO" rev-parse HEAD)" = "$EXPECTED_HEAD"
cargo run --locked --bin grokptah-cu-adaptive-eval -- \
  --out campaign-out --repeats 5 --seed 435272 \
  --repository "$REPO" \
  --expected-head "$EXPECTED_HEAD" \
  --source-gate c6f1cb23e9d6217005599850d9e0d6f7df64d5a1
```
