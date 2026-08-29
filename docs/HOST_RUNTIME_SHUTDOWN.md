# Host runtime shutdown authority

How GrokPtah decides who may write a home, and what an operator should do when
a shutdown does not release the single-instance lock. Issue #455.

## Why this exists

A GrokPtah home (`~/.grokptah` by default) is single-writer. Two processes
writing one home double-append transcripts and race garbage collection, so the
home is guarded by an advisory `flock` on `.instance.lock` (#119).

The hosted desktop soaks (PR #450 run 33129615156, PR #448 run 33131386866)
showed that holding that lock correctly is not enough. A full MCP campaign could
finish, the control server could stop, the caller could drop its host — and the
next launch on the same home would still be refused, because cloneable request
handles captured by spawned tasks kept the lock alive. The lock outlived the
runtime that was supposed to own it.

The repair is an ownership seam, not a timeout.

## The ownership model

There is exactly one owner and any number of borrowers.

- **`HostRuntime`** is the owner. It is **not** `Clone`. It holds the process
  lock and the task supervisor, and it is the only thing that can release the
  lock.
- **`AgentHostHandle`** is a request handle. It **is** `Clone`, it is what
  spawned tasks and service layers capture, and it owns no lock. Cloning it can
  never extend the lifetime of the process lock.
- **`DurableWriteGuard`** is proof that the owner still holds authority. Every
  durable mutation in the bridge takes one by reference, so "did I remember to
  check the lifecycle?" is answered by the compiler rather than by review. It is
  `#[must_use]`: producing one and dropping it on the same line is a compile
  error under `clippy -D warnings`, because a dropped guard is a *check*, and a
  check can go stale between its answer and the mutation it authorized.
- **`WriteLease`** is the authority a *store handle* carries. `OrchStore`,
  `ComputerStore` and the event journal are cloneable handles over shared
  durable state, so the lease lives in their shared inner: every clone fails
  closed together when the runtime that opened them stops.

A lease is established **once, at open**, and is one of exactly two things:

1. the lifecycle of the live runtime that owns the home, or
2. an `OfflineMaintenanceAuthority` — the same OS lock a host would take, held
   for the entire lifetime of the handle.

There is no third case. Authority is never derived from the *absence* of an
owner, and never from a momentary "is the lock held?" probe: a probe's answer
can be false by the time the write lands, which is exactly the race the lease
exists to remove.

Authority is also never borrowed **across homes**. Binding a store or journal to
a runtime verifies that the handle's canonical home is the home that runtime
holds the lock for; if it is not, the bind is refused and the handle keeps the
authority it established at open.

## Ordered shutdown

`HostRuntime::shutdown()` runs one fixed order. Each step must complete before
the next begins:

1. **Refuse new admissions.** The phase moves to `Quiescing`. Work already in
   flight keeps its authority — refusing here would strand in-flight
   finalizations and turn an orderly stop into a join timeout.
2. **Stop accepting HTTP/SSE.** Control-server accept loops are cancelled and
   joined; live SSE streams are woken so an open stream cannot block the join.
3. **Cancel and JOIN** every supervised task: runs, background work, subagents,
   Computer Use operations, watchers. Cancel is not enough — the join is the
   guarantee.
4. **Seal durable writes.** After the seal, no new `DurableWriteGuard` is ever
   issued.
5. **Flush durable state and run shutdown hooks** (audit ledger close and
   friends) — only if the seal held.
6. **Mark closed**, so every stale handle fails closed.
7. **Release the advisory lock exactly once.** The lock *file* stays on disk;
   only the advisory lock is released.

A terminal run never counts as shutdown-complete while a spawned task or guard
still holds authority. Repeated `shutdown()` calls are idempotent.

## The operator contract: uncertainty retains the lock

**When shutdown cannot prove it is safe to release the lock, it keeps it — for
the life of the process.**

Release happens only when all three hold:

- durable writes sealed, and
- the join did not time out, and
- the durable flush and every shutdown hook succeeded.

If any of those fails, `process_lock_retained_for_safety` is set, the lock is
**quarantined**, and a replacement process on that home is refused until this
process exits. Refusing a replacement is always safer than handing it a home
this process may still be writing.

Quarantine is what makes that promise true. Leaving the lock inside the runtime
would only be a decision not to release it, and that decision lasts exactly as
long as the object does — for a consuming `shutdown()`, until the call returns.
Quarantining moves the lock out of the runtime and into process ownership, where
nothing drops it: destroying the runtime, every handle, and every clone cannot
hand the home on. `quarantined_process_lock_count()` reports how many homes this
process is holding that way.

The same rule governs `Drop`. `Drop` cannot await, so it has no way to join
outstanding work; a runtime dropped with any supervised task still outstanding,
or with a durable write in flight, **retains the lock permanently for that
process**. There is no later re-check, because there is no later point at which
`Drop` could run one.

This is deliberate and it is not a bug to work around. Do not delete
`.instance.lock`, and do not add a retry loop or a sleep: both reintroduce the
two-writer hazard the lock exists to prevent.

### What an operator sees

An unclean shutdown prints one line naming the reason:

```
[grokptah] host shutdown for /Users/you/.grokptah/.instance.lock: UNCLEAN: \
  joinTimedOut=true tasksRemaining=2 writesSealed=true writesInFlight=0 \
  lockRetainedForSafety=true errors=[]
```

A drop without an ordered shutdown prints:

```
[grokptah] host runtime for … dropped without an ordered shutdown: 0 durable \
  write(s) in flight, 3 supervised task(s) outstanding. The instance lock is \
  RETAINED for the life of this process so no replacement can start beside \
  work that may still act. Await HostRuntime::shutdown() for an ordered stop.
```

The same information is available programmatically on `HostShutdownReport`
(`is_clean()`, `operator_summary()`), which the desktop and the headless service
both surface on exit.

### What to do

| Symptom | Cause | Action |
| --- | --- | --- |
| `lockRetainedForSafety=true`, `joinTimedOut=true` | supervised work did not finish inside the join budget | Exit the process. The next launch succeeds. Report the `tasksRemaining` count — a task that will not join is a bug in that task, not in shutdown. |
| `lockRetainedForSafety=true`, `writesSealed=false` | a durable write was still running when the seal gave up | Exit the process. Do not start a second one against this home. |
| `lockRetainedForSafety=true`, non-empty `errors` | the durable flush or a shutdown hook failed | Exit the process, then check the home for the named failure before relaunching. |
| "dropped without an ordered shutdown" | an embedder dropped `HostRuntime` instead of awaiting `shutdown()` | Fix the embedder to `await runtime.shutdown()`. |
| A launch is refused with "another process holds the single-instance lock" | another live process owns the home — possibly one of the above still running | Find and exit that process. Never delete the lock file. |
| A launch is refused because "a durable store under … is separately owned" | an offline maintenance handle holds a store root's own lock inside this home | Stop the process or handle holding that root; a runtime must not take a home whose stores another writer owns. |

Restarting the process always clears a retained lock, because the advisory lock
is released by the OS when the process exits. That is the intended recovery, and
it is why the failure mode is *bounded*: it costs one process restart, never a
corrupted home.

## What this does not cover

The durable-write seal governs writes to the **home**. It does not govern
effects outside it — a workspace edit, a physical provider send, Computer Use
input. Those are bound to the runtime only through the **join**: an ordered
shutdown joins the tasks that perform them before releasing the lock, and `Drop`
quarantines the lock whenever any such task is outstanding.

Coverage of that join is the current limit, so here is the actual inventory
rather than a general assurance. Every place the bridge starts work outside the
caller's own task:

| Site | Effect | On the barrier? |
| --- | --- | --- |
| `session_prompt_inner` | provider send, tool execution, workspace edits | **Yes** — registered before the turn starts, so no caller can begin one unsupervised |
| `DesktopComputerUse`: `request_permission`, `list_targets`, `observe_once`, `start_simulator`, `start_native`, `refresh_simulator`, `approve_simulator_action` | permission, enumeration, capture, run start, input delivery | **Yes** — `track_supervised` around each of these seven, and only these |
| `DesktopComputerUse`: `stage_simulator_action`, `apply_model_proposal`, `pause_simulator`, `take_over_simulator`, `stop_simulator`, `discard_simulator_approval` | durable-only; no platform call | not tracked — the durable-write guard governs them |
| orchestration run + aggregator tasks | durable runs | **Yes** — `spawn_supervised` |
| background scans, shells, subagents | workspace, child processes | **Yes** — `spawn_supervised` |
| scheduler / native / manager watchers | durable | **Yes** — supervised and joined |
| `mcp_control` axum serve task | HTTP/SSE acceptance | **Yes**, by a different mechanism: owned in `ControlServerHandle` and joined by `stop_and_wait` |
| `host_runtime` seal `spawn_blocking` | none (it *is* shutdown) | n/a |
| `host.rs` workspace walk `spawn_blocking` | read-only | n/a |
| `local_tools` stdout/stderr pumps | read pipes only; the child process is the effect, and it is tracked in `live_shells` and killed by `cancel_all_activity` | pumps: **no**; child: yes |
| `computer_use/macos_native` `spawn_blocking` calls | accessibility permission, capture, native input | **No** — see below |
| `eval_oracle` command runner | evaluation tooling, outside the host lifecycle | n/a |

Two honest gaps in that table:

1. **`spawn_blocking` is not cancellable and not joined.** The macOS native
   Computer Use calls run inside a supervised future, so shutdown waits for the
   *awaiter* — but if that awaiter is cancelled, the blocking task itself keeps
   running to completion. A native input or capture can therefore outlive the
   join by the length of one platform call.
2. **Anything a future contributor adds with a bare `tokio::spawn`** is outside
   the barrier by default. There is no compile-time enforcement here, unlike the
   durable-write guard.

Binding every external effect to the same lifecycle is tracked separately
(#454, #463); until then the join is a proxy for "no effect can still happen",
not a proof of it.

Also unproven, and deliberately not claimed:

- **Windows.** All of this is `flock` semantics on Unix. Windows lock, ACL and
  reparse-point behaviour — including whether a refused contender can affect the
  owner's lock file, and how a killed process's lock is reclaimed — is untested.
- **Symlinked lock path.** `.instance.lock` still follows a pre-existing
  same-user symlink; there are no `O_NOFOLLOW`/`openat` identity checks.
- **Soak qualification.** Crash recovery is proven for one killed owner
  (`a_killed_owner_leaves_a_takeable_home_and_a_readable_ledger`). That is not a
  soak: repeated kills under concurrent load, and kills landing inside a
  partially written durable record, are not covered here.
