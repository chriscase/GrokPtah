# Synthetic campaign summary (seed 435272, repeats 5)

Status: **PASS**

Exact evaluated code head: `3cbd470e17597258293c3190f6eb95d26a03b5df`

Exact evaluated tree: `b14c677400039e1abceca6614171080e0f437854`

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
| Campaign digest | f3965680c03a9b22a197ef7e934a558c0b25ad7e4611ffbfc0e737696315afcc |

Regenerate:

```sh
REPO="$(git rev-parse --show-toplevel)"
cargo run --locked --bin grokptah-cu-adaptive-eval -- \
  --out campaign-out --repeats 5 --seed 435272 \
  --repository "$REPO" \
  --expected-head 3cbd470e17597258293c3190f6eb95d26a03b5df \
  --source-gate c6f1cb23e9d6217005599850d9e0d6f7df64d5a1
```
