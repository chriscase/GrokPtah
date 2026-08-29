# Synthetic campaign summary (seed 435272, repeats 5)

Status: **PASS**

Source gate: `67e29bd34dc64049432c715c93c2cef2185c63ea`

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
| Fixture hash | 3dc08fca75ccdd0c343d646aafe88bcf90a2715a72c87ed187802adbb97f1110 |

Regenerate:

```sh
cargo run --locked --bin grokptah-cu-adaptive-eval -- --out campaign-out --repeats 5 --seed 435272
```
