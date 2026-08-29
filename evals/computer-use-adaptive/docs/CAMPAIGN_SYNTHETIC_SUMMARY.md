# Synthetic campaign summary (seed 435272, repeats 5)

Status: **PASS**

Exact evaluated code head: `270102554529e1995ec79e095aaa807250be26fb`

Exact evaluated tree: `9ca2e6b1dd1c60b1b8a9f39fe4a1e39653442f5d`

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
| Campaign digest | 71c09944d0e9da769563063f71ff4e48a1217ecae68f77ea5bf0b88c792a4415 |

Regenerate:

```sh
REPO="$(git rev-parse --show-toplevel)"
cargo run --locked --bin grokptah-cu-adaptive-eval -- \
  --out campaign-out --repeats 5 --seed 435272 \
  --repository "$REPO" \
  --expected-head 270102554529e1995ec79e095aaa807250be26fb \
  --source-gate c6f1cb23e9d6217005599850d9e0d6f7df64d5a1
```
