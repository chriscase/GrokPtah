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
| Fixture hash | 614a8b4b0bf5d5f559764f894661475a11e75e1e40279bdbe5e48cf5387cc20a |
| Campaign digest | 2fce11ff4e0de769267f4b22555a23029b34f7e3944679afffb1881489e74198 |

Regenerate:

```sh
cargo run --locked --bin grokptah-cu-adaptive-eval -- --out campaign-out --repeats 5 --seed 435272
```
