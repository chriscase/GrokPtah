# Durable routines and activations

Issue #306 adds the first transport-neutral routine contract to
`grokptah-agent-bridge`. A routine is a trigger definition. An activation is
one attributable attempt to make Work eligible through the existing durable
workload API. Routines do not own execution, leases, retries of model turns,
approvals, or promotion.

This is the common contract for a local desktop, a local service, and a hosted
service. Those hosts must not keep a second scheduler or a second Work queue.

## Ownership

| Record | Durable responsibility |
| --- | --- |
| `RoutineRecord` | Trigger, owning Agent, Work template, missed-run/concurrency/retry policy, lifecycle, next-fire |
| `ActivationRecord` | One occurrence/dedupe key, captured policy/bounds revision, resulting Work ID, disposition |
| `WorkItem` | Eligible work created by an activation (`sourceRoutineId` / `sourceActivationId`) |
| Runtime-home owner | The single process that holds `GROKPTAH_HOME` fires due routines |

Desktop UI timers are display/request only. They are not authoritative.

## State machine

Routine lifecycle:

```
enabled  --pause-->  paused  --enable-->  enabled
enabled  --disable--> disabled --enable--> enabled
paused   --disable--> disabled
```

- `enabled`: scheduled ticks and manual fire are admitted.
- `paused`: scheduled ticks are recorded as `skipped_paused`. Manual fire still
  creates Work so an operator can intervene without unscheduling the definition.
- `disabled`: scheduled and manual fire are recorded as `skipped_disabled`.
  `nextFireAt` is cleared until enable.

Activation dispositions:

| Disposition | Work created? | Meaning |
| --- | --- | --- |
| `created_work` | yes | Eligible Work was written through the workload store |
| `deduplicated` | no | Same durable occurrence key already produced an activation |
| `skipped_paused` / `skipped_disabled` | no | Lifecycle refused the cause |
| `skipped_overlap` | no | In-flight Work already meets `maxInFlight` |
| `skipped_missed` | no | Skip policy discarded stale slots after downtime |
| `skipped_expired` | no | One-shot `expiresAt` elapsed |
| `rejected` / `backoff` / `circuit_open` | no | Validation or repeated failure; circuit pause is explicit |

An activation never:

- resumes an interrupted model invocation
- claims a lease
- approves a tool, permission, or Computer Use request
- promotes code
- widens captured Agent authority

Computer Use and automatic tool approval are forced false on every captured
policy snapshot.

## Delivery guarantees

Delivery is **at-least-once with durable deduplication**.

- Scheduled occurrence key: `sched:<routineId>:<occurrence RFC3339 millis UTC>`
- Manual key: `manual:<routineId>:<requestId>`
- The same key returns the original `ActivationRecord` and does not create a
  second Work item
- MCP `request_id` replay is a second, independent idempotency ledger for the
  control-plane call itself

Crash recovery uses `routine-intents/`. Opening the store commits any leftover
intent (Work + activation + dedupe) before applying other recovery.

## Missed-run policy

After downtime, `due_occurrences` inspects every slot from `nextFireAt`
through now:

- **skip** (default): if more than one slot was missed, create no Work and
  advance to the next future slot. A single still-current slot is fired.
- **coalesce**: create exactly one activation for the latest missed slot.
- **catch_up**: create one activation per missed slot, newest first, capped at
  eight. Catch-up still obeys `maxInFlight`; extra slots are recorded as
  `skipped_overlap` rather than a second queue.

One-shot triggers use `expiresAt` when present. An expired one-shot never
creates Work.

## Timezone and daylight saving

Interval triggers use a UTC duration from an explicit anchor. The timezone
name is validated so later calendar conversions stay attributable.

Calendar triggers use an IANA timezone:

- Spring-forward gaps (`02:30` on a US DST start) are skipped; the next valid
  local wall time is used.
- Fall-back overlaps fire once, at the earlier offset.

## Retry and backoff

Activation-time failures (unknown Agent, workspace mismatch, invalid bounds)
increment `consecutiveFailures` and move `nextFireAt` forward by
`backoffMs * 2^(n-1)`, capped at `maxBackoffMs`. Reaching `circuitFailures`
pauses the routine with `circuitOpen`. Enable clears the circuit. Work-item
retry remains the workload engine's job.

## MCP / service surface

Read:

- `ptah_list_routines`
- `ptah_get_routine`
- `ptah_list_activations`

Mutate (idempotent `request_id`):

- `ptah_create_routine`
- `ptah_fire_routine`
- `ptah_pause_routine`
- `ptah_enable_routine`
- `ptah_disable_routine`

`ptah_get_capacity.health.routineSupervisor` reports the runtime-home tick
loop. `/ready` fails closed when that loop's latest pass records an error.

## Extension boundary

`RoutineTrigger::External { adapter: webhook | github | message }` is part of
the durable schema so later adapters can share this activation interface. This
slice rejects creating or firing those adapters with `unsupported`. Adding an
adapter must not change the Work state machine.

## Minimal prerequisite used by this slice

`WorkItem` gained optional `sourceRoutineId` / `sourceActivationId`. Existing
items deserialize as absent. `WorkItem::new_at` exists so fake-clock tests can
stamp deterministic timestamps. No change to issues #297, #299, or #300 was
required; Agent spec revisions, token ceilings, and ADR-002 authority rules
are consumed as already shipped.

## Follow-up work

1. Webhook adapter: authenticated ingest, signature, replay window, payload
   schema, and `External { Webhook }` firing through `ActivationRequest`.
2. GitHub adapter: installation identity, event filters, and the same
   activation seam.
3. Durable message adapter: agent/user messages as trigger occurrences.
4. Desktop schedule editor for calendar/interval triggers (still request-only).
5. Per-principal authorization so routine mutation is not operator-equivalent
   for every bearer.
