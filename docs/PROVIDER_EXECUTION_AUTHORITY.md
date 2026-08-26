# Provider execution authority and durable continuation

Every physical provider request an Agent makes is a privileged action. It
spends the account's money, speaks with the account's credential, and can act
on the repository the Agent is bound to. This document defines the durable
contract that makes that authority explicit, and the boundaries that refuse a
request fail-closed.

The contract lives in `orchestration::provider_authority`. It reuses the
existing certification route/credential classes and the existing
`OrchError`/`OrchErrorCode` wire contract; it does not introduce a second
control-plane error vocabulary or a second run state machine.

## Invariants

1. **Bound authority.** An attempt is admitted only against an
   authority-owned `ProviderAuthorityScope`: account, tenant, installation,
   Agent, run, Lane, frozen specification revision, provider route class and
   endpoint fingerprint, credential class and credential binding digest,
   model, and repository workspace/ref/policy digest. A binding that is
   missing a field, disagrees with the authority-owned scope, has expired, or
   replays a request fingerprint is denied.
2. **Single-use confirmation.** Transport is gated on a `ConfirmationGrant`
   that is audience-checked against the exact binding digest, subject-checked
   against the exact attempt, expiry-checked, and nonce-checked in constant
   time. Consumption is written durably *before* the transition it authorizes,
   so a restart in the middle of a send can lose the send but never the fact
   that the confirmation was spent.
3. **Honest delivery state.** Follow-up intent, cancel intent, and the
   host-generated request identity are persisted before transport. The durable
   record therefore always distinguishes `known_not_sent`, `sending`,
   `uncertain`, and `settled`.

## Send-state lattice

```text
known_not_sent ──confirmation consumed──> sending ──response observed──> settled
      │                                      │
      │                                      └──delivery unknowable──> uncertain
      │                                                                    │
      └──abandoned / cancelled──> settled          explicit reconciliation ┘
```

Only `known_not_sent` reports `auto_retry_allowed`. `uncertain` is never
automatically retried: re-sending could double-charge the account or duplicate
a side effect the provider already applied. The only exit from `uncertain` is
`resolve_uncertain`, which requires a reconciliation outcome, a bounded
host-authored evidence code, and an attributed resolver.

A `continuation_key` names the logical provider request. While an `uncertain`
attempt holds a key, a new attempt on that key is denied with
`uncertain_attempt_not_retryable` — even though a legitimate retry
re-fingerprints and would otherwise pass the replay guard.

The lattice also refuses states that would be dishonest about delivery:

- an attempt that never started transport cannot settle as `delivered` or
  `rejected`;
- an attempt that started transport cannot settle as `abandoned_before_send`;
- a provider request identity cannot be admitted, or recorded, before
  transport starts, because the provider has not been spoken to yet.

## Denial boundaries

Each denial travels as `error.data.denial` alongside an existing
`OrchErrorCode`, so a coordinator can branch on the exact boundary that
refused without a wider error enum.

| Denial | Code | Refused because |
| --- | --- | --- |
| `binding_missing` | `invalid_request` | A required binding field was absent, blank, oversized, or NUL-bearing. |
| `binding_mismatch` | `forbidden_scope` | Agent, run, or Lane identity disagrees with the authority-owned scope. |
| `binding_stale` | `stale_version` | Binding expired, is not yet valid, or froze a superseded specification revision. |
| `binding_replayed` | `conflict` | The request fingerprint was already claimed, in this run or any other. |
| `route_not_authorized` | `forbidden_scope` | Route class, endpoint fingerprint, credential class, or credential binding disagrees. |
| `tenant_mismatch` | `forbidden_scope` | Account, tenant, or installation boundary crossed. |
| `repository_mismatch` | `workspace_mismatch` | Workspace, ref, or policy digest boundary crossed. |
| `model_mismatch` | `forbidden_scope` | Model outside the frozen Agent route. |
| `grant_missing` | `forbidden_scope` | Transport attempted with no confirmation grant. |
| `grant_subject_mismatch` | `forbidden_scope` | Grant was minted for a different attempt. |
| `grant_audience_mismatch` | `forbidden_scope` | Grant audience is not this binding's digest. |
| `grant_expired` | `forbidden_scope` | Grant is past its expiry. |
| `grant_nonce_mismatch` | `forbidden_scope` | Presented nonce does not match, or is below the entropy floor. |
| `grant_already_consumed` | `conflict` | Grants are single-use, across restart. |
| `send_state_transition_invalid` | `invalid_request` | Transition is not part of the lattice, or a cancel intent is pending. |
| `uncertain_attempt_not_retryable` | `conflict` | An `uncertain` attempt may only be reconciled explicitly. |
| `continuation_key_busy` | `conflict` | A live attempt already holds this logical request. |
| `attempt_unknown` / `attempt_already_exists` | `invalid_request` / `conflict` | Attempt identity does not resolve, or collides. |
| `ledger_bound_exceeded` | `capacity_exhausted` | Run reached `MAX_PROVIDER_RECEIPTS_PER_RUN`. |

## Durable layout and restart

The ledger is a separate durable domain below the orchestration store root:

```text
<store>/provider-authority/
  attempts/<sha256(runId)>/<sha256(attemptId)>.json
  grants/<sha256(runId)>/<sha256(grantId)>.json
  fingerprints/<sha256(requestFingerprint)>.json
  continuation-holders/<sha256(runId)>/<sha256(continuationKey)>.json
```

Attempt and grant writes are atomic (temp file, `fsync`, rename, directory
`fsync`). Fingerprint claims and grant issuance use an exclusive hard-link
install, so two racing writers cannot both be admitted.

`ProviderAuthorityLedger::open` runs restart recovery: every attempt left
`sending` becomes `uncertain` with reason `restart_during_transport`. Recovery
is idempotent. An attempt left `known_not_sent` stays retryable, because the
host genuinely knows nothing was transmitted. A record that will not parse
fails closed on read rather than reading as absent.

## Receipt projection

`ProviderAttemptReceipt` (`grokptah.provider_attempt_receipt.v1`) is bounded
and secret-free. It carries `attemptId`, `clientRequestId`, `providerRequestId`
when known, `sendState`, `autoRetryAllowed`, `confirmed`, the follow-up and
cancel disposition, the settled outcome or uncertainty reason, and an
`AuthorityBindingSummary`.

Raw account, tenant, installation, and workspace values never appear; they are
reduced to domain-separated digests so two receipts can still be compared.
Route class, credential class, model route, repository ref, policy digest,
binding digest, and request fingerprint appear verbatim because they are
already public classification. The receipt is validated in test against
`certification::scan_value_for_forbidden_data`.

`unknowns` is derived from the send state rather than stored, so it can never
drift: a reader is told explicitly that delivery, provider outcome, usage, or
the provider request id is not established, instead of inferring it from an
absent field.

## Deriving an authority scope

`resolve_authority_scope` is the only supported way to obtain a
`ProviderAuthorityScope` on the production path. It derives the scope from
durable records alone — `AgentRecord`, its current `AgentSpec`, the
`RunRecord`, and host-resolved route facts — and refuses when they disagree:

- an Agent with no claimed `ownerPrincipalId` cannot authorize a request;
- a run not owned by the Agent, or that never froze this specification
  revision, is refused;
- a run whose workspace disagrees with the specification workspace is refused;
- a Lane not associated with the Agent is refused;
- an isolated run pins its recorded base revision as the repository ref.

### Single-tenant projection

GrokPtah has no multi-tenant service yet. `installation_identity` derives a
stable installation from the durable store root, and `single_tenant_identity`
projects the account as its own tenant. These are deliberate projections, not
an inferred hierarchy: every binding, digest, receipt, and denial boundary
already carries the fields, so a real multi-tenant service supplies genuine
values without changing the wire or storage shape.

## Known gaps

- The ledger has no retention policy. Per-run attempts are bounded by
  `MAX_PROVIDER_RECEIPTS_PER_RUN`, but the number of runs is not, and a
  fingerprint claim written just before a failed attempt write is never
  reclaimed. Any future pruning must never remove an `uncertain` attempt or an
  unconsumed grant, since both are still reconciliation evidence.
- `settle_attempt` accepts `cancelled` from `sending` on the caller's
  assertion that the request never left. When that is not actually known, the
  honest call is `mark_uncertain`, not a cancelled settle.

## Not yet wired

The ledger is not yet engaged by the live provider transport path in
`host.rs`. `RunUsageTracker` still admits attempts as an unnamed pending
counter (`aggregates.usagePendingRequests`). Wiring is deliberately deferred
because it needs product decisions this contract does not make: who issues a
confirmation grant for an ordinary desktop turn, what repository ref a
shared-mode run pins, and whether each provider round needs its own
confirmation or one per turn.

The intended wiring order, once those are decided:

1. Resolve a scope in `RunUsageTracker::from_run` via
   `resolve_authority_scope`, and carry it on the tracker. Runs with no Agent
   identity keep today's behavior and report an absent authority binding
   explicitly rather than a fabricated one.
2. Replace the unnamed pending counter in `begin_attempt` with a
   `ProviderAuthorityLedger::begin_attempt`, keeping
   `aggregates.usagePendingRequests` in sync so existing accounting and its
   `token_accounting_unavailable` stop are unchanged.
3. Gate the actual send on `begin_transport`, and map the existing transport
   outcomes onto `settle_attempt` / `mark_uncertain`. A transport error whose
   request bytes were already committed becomes `uncertain`, not a retry.
4. Reconcile `OrchStore::open`'s `mark_unfinished_interrupted` with
   `ProviderAuthorityLedger::open`'s recovery so an interrupted run and its
   in-flight attempts agree after a restart.
5. Expose `receipts` through a read-only control tool, and add its schema to
   `CONTROL_TOOLS`.
