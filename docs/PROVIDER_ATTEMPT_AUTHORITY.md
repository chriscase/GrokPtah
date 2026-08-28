# Provider-attempt authority

GrokPtah has one durable authority for provider completion writes:
`xai-provider-attempt::ProviderAttemptStore`. Observation and usage records may
describe a request, but neither can authorize a physical send.

## State-transition matrix

| Current | Valid next state | Meaning |
| --- | --- | --- |
| `Prepared` | `Admitted`, `Cancelled` | Intent is durable; no provider I/O is permitted yet. |
| `Admitted` | `Sending`, `Cancelled` | Host admission and authority checks succeeded. |
| `Sending` | `Responding`, `Uncertain`, `Failed` | A write permit exists. A response started, ambiguous loss, or known pre-effect failure was recorded. |
| `Responding` | `Settled`, `Uncertain` | The same attempt owns stream start and final settlement; an incomplete response is ambiguous. |
| `Uncertain` | `Admitted` or `Settled` only through explicit reconciliation | Provider truth is required. Retry code cannot reopen it. |
| terminal | none | `Settled`, `Failed`, and `Cancelled` are absorbing. |

`Uncertain -> Admitted` requires operator authorization plus provider truth that
the request was not applied. `Uncertain -> Settled` requires an external
provider settlement proof. The ledger stores no request or response body.

The provider request key is derived once from the host operation, provider,
request fingerprint, and durable attempt ID. The exact key is persisted before
`Sending` and remains unchanged across credential refresh, restart, and
explicit no-effect reconciliation. Adapters transmit it as
`Idempotency-Key` and `x-grok-req-id` when the provider route supports
idempotency.

## Authority binding

Each attempt stores the host snapshot of principal incarnation/generation,
capability generation, and effect lease. The host re-reads those canonical
authorities after waits and immediately before the physical request. A stale
snapshot is terminally rejected before socket I/O.

Public projections contain only `attemptId`, `sendState`, and
`providerRequestId`. Raw request keys, authority values, credentials,
endpoints, bodies, paths, and diagnostics are not public projection fields.
