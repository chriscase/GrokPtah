# Headless host

`grokptah-headless-host` is the runnable seam for starting, observing, steering,
pausing, resuming, and durably recovering GrokPtah runs without the desktop app.

It is a **library plus one binary** (`grokptah-headless`). It owns process
lifecycle, durable state, authority enforcement, and projection. It does not own
model execution, credentials, or transport.

## What this is not

This is not the "headless service" that [ADR-002](./ADR-002-runtime-boundaries.md)
withholds, and it does not make the desktop bridge run without Tauri. It is a
separate host with a home of its own, an injected engine port, and no ambient
access to anything the desktop owns. The bridge remains the desktop authority
anchor.

The engine that ships today is the deterministic offline fixture engine. Wiring
a real provider-backed engine is a separate change with its own credential,
sandbox, and approval questions; see [Residuals](#residuals).

## Authority tier

ADR-002 §3 requires every surface to name its tier, scope, mutation authority,
redaction contract, idempotency behavior, and recovery behavior.

| Question | Answer |
| --- | --- |
| Tier | Local host authority for one home it owns exclusively |
| Scope | One session identity, one approved workspace, one home directory |
| May | Admit and bound its own runs; hold short-lived control leases; recover its own interrupted runs; publish redacted projections |
| May never | Write another GrokPtah home; originate work by itself; hold or read credentials; grant Computer Use; resume interrupted work without an explicit operator action; widen a capability it was not configured with |
| Redaction | Every value is scrubbed at the write boundary, before it is journaled and again before it is projected |
| Idempotency | A durable ledger keyed by `requestId`; an exact retry replays, a changed payload is refused |
| Recovery | Opening the store marks queued/running runs `interrupted`; they are never auto-resumed |

### Which ADR-002 trigger this satisfies

ADR-002 §2 lists three service triggers. This host satisfies **the first** — work
that must survive the operator's terminal exiting — and deliberately not the
other two:

- It takes an **exclusive lock on a home of its own**, so there is never a second
  concurrent writer to one home. Configuration refuses a home whose final path
  component is `.grokptah`, and the lock refuses a second host regardless.
- It accepts **no off-box caller**. The control protocol is newline-delimited
  JSON on the process's own stdin/stdout. There is no listening socket, no new
  identity model, and no new authentication model.

## Capability vocabulary

The host enforces the identifiers the desktop authority already advertises. It
mints none of its own.

| Operation | Capability | Notes |
| --- | --- | --- |
| `health`, `capabilities`, `status`, `events`, `attention` | `session.observe` | Read-only |
| `submit`, `cancel`, `pause`, `tick`, `shutdown` | `run.execute` | Bounded run mutations |
| `steer` | `run.queue` | Same tier as the desktop's steering surface |
| `resume` | `agent.resume` | Explicit manual continuation |
| `receipt` | `run.review` | Read-only review projection |
| `resolveAttention` → `deny` | `run.execute` | Denying only stops work |
| `resolveAttention` → `allow` | `run.promote` | Human-gated, so it is held to the gated identifier rather than a weaker new one |

Every capability is denied unless the configured set advertises it as
`available`, or advertises it as `gated` **and** `grants` names it explicitly.

## Fail-closed defaults

Absence is never permission:

- an unconfigured or `unavailable` capability is denied;
- a `gated` capability without an explicit grant is denied;
- a request above a host ceiling is **refused**, not silently clamped;
- an unresolved escalation expires to **deny**, never to allow;
- an unknown request or configuration field is rejected, not ignored;
- a projection the public contract would reject never leaves the host;
- a completion naming an absolute or traversing path fails the run instead of
  publishing it;
- a journal record that is unreadable anywhere but the torn tail fails the open.

## Run lifecycle

```text
queued ──▶ running ──▶ completed | failed | limit_reached
   │          │
   │          ├──▶ paused          (operator pause, or graceful shutdown)
   │          └──▶ needs_attention (escalation; allow → queued, deny → failed)
   └──────────┴──▶ interrupted     (restart recovery only; never auto-resumed)
```

`paused`, `needs_attention`, and `interrupted` are host phases that all project
to the public `interrupted` state, because a consumer sees the same thing: a run
that is halted until an operator acts. The status projection always carries the
exact phase beside the public state, so the narrower public value never hides
which one it is.

## Control leases

Observation needs only a capability. Changing a run in flight also needs a
lease: an expiring grant bound to one run scope, one set of control classes, and
the run revision the operator actually observed. Every state change bumps the
revision, so a stale steer or a replayed pause is refused rather than landing on
a run that has since moved on.

Leases live in memory only. A restart invalidates every outstanding lease, so
control authority cannot outlive the process that issued it. **This lease grants
no Computer Use authority**; that surface has its own contract and is not
implemented here.

## Orchestrator adapter

The host owns lifecycle, durability, authority, and projection. Deciding what to
say to a provider, saying it, and recording what came back belong to the agent
loop that already does that work. `orchestration::TurnOrchestrator` is the seam
between the two, and it is one-directional: the host defines the port, an
orchestrator implements it, and nothing in this crate reaches into provider code.

```text
HeadlessHost ─▶ RunEngine ─▶ OrchestratedEngine<T> ─▶ T: TurnOrchestrator
                                     │                        │
                        binding + cancellation        the existing agent loop
```

What it deliberately is **not**:

- **Not a second runtime.** The port is synchronous. An orchestrator that is
  internally async blocks on its own executor inside `run_turn`; this crate
  starts no runtime and holds no lock across the call.
- **Not a second send machine.** Nothing here dispatches anything, and the host
  never decides that a request may be repeated.
- **Not a second authority or identity model.** The orchestrator states the
  session and workspace it is bound to. The adapter's only authority decision is
  to refuse when that binding disagrees with the run.

### Referenced, not restated

An orchestrator that talks to a provider already records what was bound, what
was presented, and how far the answer got. That contract stays where it is. A
`TurnReceipt` carries only opaque `ExternalRef` handles to those records, plus
the one classification the host must act on itself.

| Disposition | Means | Host does |
| --- | --- | --- |
| `local` | Nothing left this host | Advance |
| `not_dispatched` | Prepared, provably did not leave | Advance |
| `resolved` | Dispatched and durably settled | Advance |
| `indeterminate` | Cannot be established either way | Halt and escalate |

Coarsening delivery state to four values is a projection for the host's own
decision, not a competing state machine, and its safe default is "cannot tell".
A reference that is not bounded and opaque is not recorded, and its dispatch
settles `indeterminate`: an unusable reference cannot be reconciled, and
treating it as clean would hide that.

### Refusals

| Refusal | Means | Host does |
| --- | --- | --- |
| `NotConfigured` | Nothing is wired up; waiting will not help | Fails the run |
| `Unavailable` | Configured but busy, offline, or breaker-open | Halts with an escalation an operator can allow |

Both dispatched nothing, so neither can duplicate work. They differ only in
whether an operator can fix the cause.

### Write-ahead dispatch

Before the engine is invoked, the run's record gains a dispatch entry — ordinal,
round, start time, no settlement. Only after the step returns is that entry
settled. Writing it afterwards would make a dispatch interrupted by a crash
indistinguishable from one that never started, and those two need opposite
handling.

The ordinal is durable *before* the turn runs and is passed to the orchestrator,
so an implementation can derive its own idempotency from the exact record the
host will hold if this process dies mid-turn.

On the next start, a dispatch with no settlement becomes `indeterminate`,
whatever phase the record claims, and the run is interrupted with a
`dispatch_uncertain` escalation. From there the host refuses every path that
would repeat the round:

- `resume` → `dispatch_indeterminate`
- `resolveAttention` → `allow` → `dispatch_indeterminate`
- ticking → never dispatches again

`cancel` and `resolveAttention` → `deny` remain available, because both stop work
rather than repeat it. To continue, reconcile the attempt with the orchestrator
and submit a **new run** with a fresh `requestId`. The host will not pretend to
know whether the previous attempt ran.

### Cancellation

Every step receives a `CancelSignal`. The host core is synchronous, so while a
step is running the control loop is inside it — a long turn cannot be
interrupted by an operator command, only by this channel, which the OS signal
watcher trips on an immediate stop. Cancellation observed *before* dispatch
halts the run as recoverable rather than failing it: nothing went out, so there
is nothing to reconcile and nothing to throw away.

### Integrating a real orchestrator

Nothing in the bridge or in provider code changes to adopt this. The owning lane
adds one file:

1. Depend on `grokptah-headless-host` and implement `TurnOrchestrator` for the
   existing agent-loop entry point. `run_turn` blocks on the loop's own runtime.
2. Return `binding()` from the session and workspace the loop is already scoped
   to. The workspace value is the host's alias — the workspace directory's own
   name — never a path.
3. Map the loop's existing send state to a `DispatchDisposition`. Anything not
   provably delivered or undelivered maps to `indeterminate`.
4. Put the attempt and receipt identifiers the loop already records into
   `TurnReceipt::attempt` and `TurnReceipt::receipt` as `ExternalRef`s.
5. Construct the host with `OrchestratedEngine::new(orchestrator)` instead of the
   fixture engine. Configuration keeps `engine: { "kind": "disabled" }`; the
   engine is injected, not discovered.

Step 3 is the only judgement call, and it is the owning lane's to make: it is the
component holding the evidence.

## Replay and recovery

Events are append-only NDJSON, flushed before the caller is told the event
happened. Retention is a real bound: past the window the oldest events are
compacted away and a cursor into that region is answered with
`cursorExpired: true` plus a `RunNotification::Recovery` naming the operation to
poll — never with a silent gap.

A graceful stop checkpoints live runs to `paused`, so the next start finds
resumable work. An immediate stop leaves them live on disk, so the next start
marks them `interrupted`. The difference is deliberately visible in
`startupReport.recovery`.

Because the full prompt is never durable — only a bounded redacted preview —
resuming a run recovered from a restart requires the operator to restate the
prompt. This is the same manual-resume rule
[`PERSISTENT_AGENT_PROTOCOL.md`](./PERSISTENT_AGENT_PROTOCOL.md) already applies.

## Redaction

Redaction runs at the **write** boundary, so an unredacted value is never
durable, and again before any projection is returned. It rewrites the host home
and workspace roots to `<home>` and `<workspace>`, masks credential-shaped runs
(issuer-prefixed tokens, `NAME=value` secret assignments, `Authorization` and
`Bearer` values), replaces secret-named object keys whole, strips control
characters, and bounds string size, array width, and nesting depth. Dropped
content is reported (`<omitted:N more>`), never silently discarded.

Durable records carry the workspace **alias** — its own directory name — not its
absolute path, so no host path reaches a record, an event, or a projection.

## Operating it

Configuration is JSON with no implicit discovery. `GROKPTAH_HEADLESS_HOME`,
`GROKPTAH_HEADLESS_WORKSPACE`, and `GROKPTAH_HEADLESS_SESSION_ID` override the
file, and the result is re-validated.

```sh
grokptah-headless config-check --config ./headless.json   # validate, print redacted settings
grokptah-headless capabilities  --config ./headless.json   # what this host may honor
grokptah-headless health        --config ./headless.json   # readiness + restart recovery
grokptah-headless serve         --config ./headless.json   # run it
```

`serve` reads one JSON request per line on stdin and writes one reply per line on
stdout; diagnostics go to stderr. `SIGTERM` or `Ctrl+C` drains and checkpoints; a
second signal stops immediately. On Windows, console `Ctrl+C` is the whole signal
surface, because there is no `SIGTERM`.

```jsonc
{"id":"1","command":{"op":"submit","requestId":"req-1","prompt":"...","allowQueue":true}}
{"id":"2","command":{"op":"lease","runId":"run-…","classes":["steer"],"expectedRevision":3}}
{"id":"3","command":{"op":"steer","runId":"run-…","leaseId":"lease-…","expectedRevision":3,"directive":"..."}}
{"id":"4","command":{"op":"events","runId":"run-…","afterSeq":12,"limit":64}}
```

`health` on a home no host owns performs restart recovery, which is exactly what
the next `serve` would do and is idempotent. On a home another host owns it
reports `ownedElsewhere` and touches nothing.

## Verification

```sh
cargo test  -p grokptah-headless-host --all-features --locked
cargo fmt   -p grokptah-headless-host -- --check
cargo clippy -p grokptah-headless-host --all-targets --all-features --locked
```

Everything is offline and deterministic: temporary homes, a fixed clock, and the
scripted fixture engine. No provider credential, no network, no wall-clock sleep.

## Residuals

- **No provider-backed orchestrator.** `TurnOrchestrator` has one
  implementation in tree, the deterministic offline fake. The adapter is ready;
  binding it to the real agent loop is the owning lane's step, and the
  credential, sandbox, and approval questions come with it.
- **A long turn blocks the control loop.** The port is synchronous by design, so
  while a turn runs the host processes no operator command. Cancellation reaches
  it through the step's `CancelSignal`; everything else waits. Making turns
  concurrent would mean more than one run in flight per host, which is a
  different authority question.
- **No Computer Use.** The control lease deliberately grants none, and no
  Computer Use surface is implemented here.
- **Single session and workspace per host.** Multi-workspace hosting would need
  a scope model this host intentionally does not have.
- **No transport.** Control is stdio only. A network control plane would be an
  off-box originator and needs the separate identity and security work ADR-002
  §3 requires.
- **Health probes the home lock.** `HomeLock::is_held` takes and releases the
  lock, so it can race a host that is starting. Health reports it as observed
  state, never as authorization.
