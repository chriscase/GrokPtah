# Durable audit generations (`grokptah-audit.v2`)

`crates/codegen/grokptah-agent-bridge/src/audit/` implements the durable,
append-only, tamper-evident audit authority used by long-running agents and
embeddable consumers (#443), and `OrchStore` writes through it (#462).

**The shipped orchestration store uses v2.** There is no second ledger and no
dual write: the retired v1 files are read-only inputs that were imported into
the leading generations and are never opened for writing again.

This document describes the format, the crash-cut contract, and — just as
importantly — the limits it does **not** close.

## Why it exists

The shipped orchestration audit ledger (`orchestration/store.rs`) appends to
`audit/audit.jsonl` and, at 4 MiB, deletes `audit.jsonl.1` and renames the
current file onto it. Three consequences follow:

1. The third-oldest generation is destroyed with no manifest, tombstone, marker
   or audit record. Nothing anywhere states that it existed.
2. A crash between the delete and the rename loses the previous generation
   while the current one survives, so the surviving state looks correct.
3. A crash between the rename and the first append leaves no `audit.jsonl`, so
   "rotated a moment ago" and "never audited anything" are indistinguishable.

Entries carry no sequence and no MAC, so truncation, reordering, deletion and
wholesale substitution are all undetectable. A dropped entry sets an in-memory
error that dies with the process.

## Where it lives

```
<orchestration-root>/audit/
  audit.jsonl            retired v1 bytes, read-only after the cutover
  audit.jsonl.1          retired v1 bytes, read-only after the cutover
  audit.key              installation key for the default local-file custody (0600)
  v2/                    the authority (layout below)
```

`OrchStore::open` resolves key custody, opens the ledger, imports any v1 bytes
on first use, and refuses to start if the audit cannot be authenticated. A
store whose audit is poisoned does not open: that is the fail-closed direction
and it is deliberate, because an orchestration host that cannot record what it
did should not do anything.

## Layout

```
<root>/
  manifest.json          authenticated; names the active generation. THE pointer.
  manifest.json.tmp      transient only; a reader never promotes it
  gap.json               authenticated durable dropped-entry evidence
  bootstrap.json         present only during an uncommitted first-open import
  generations/
    g-000001/  journal.jsonl  anchor.json
    g-000002/  journal.jsonl  anchor.json
```

Files are `0600`, directories `0700`, and a symlinked root is refused.

## Documents

| Document | Schema | Authenticated with | Rewritten |
| --- | --- | --- | --- |
| Manifest | `grokptah-audit-manifest.v2` | `K_manifest` | on rotation, retention, recovery convergence |
| Anchor | `grokptah-audit-anchor.v2` | `K_anchor` | on every append |
| Gap file | `grokptah-audit-gap.v2` | `K_anchor` | when entries are dropped |
| Export | `grokptah-audit-export.{v1,v2}` | `K_seal` | never (a copy) |
| Journal line | `v: 2` records | `K_chain` | append-only |

All are `deny_unknown_fields` and carry a MAC over the canonical bytes of
themselves minus that MAC. Canonical JSON sorts object keys explicitly and
rejects non-integer numbers, so a MAC never depends on `serde_json` feature
unification elsewhere in the dependency graph.

Keys are derived from one installation key by domain-separated HMAC
(`chain`, `manifest`, `anchor`, `seal`, `actor`). HMAC-SHA256 is implemented
over the crate's existing `sha2` dependency, so the bridge lockfile is
unchanged.

## Continuity rules

| ID | Rule |
| --- | --- |
| C1 | `seq` is global to the ledger instance, rises by exactly 1 per committed entry, and never resets — across rotation, restart, key rotation, retention and export. |
| C2 | `gen[i+1].firstSeq == gen[i].lastSeq + 1`, enforced at commit and re-verified at open. |
| C3 | `gen[i+1].chainBase == gen[i].finalTag`, MAC-verified. |
| C4 | The entry tag is `HMAC(K_chain, prev ‖ canonical(record))` over a record containing its own `gen` and `seq`, so renumbering an entry or moving one between generations is detectable. |
| C5 | Renaming a journal file is never a rotation. A path never changes meaning. |
| C6 | A journal is never truncated, except a byte-exact unterminated trailing run during recovery — bounded, recorded as `recovery.torn_tail`, and impossible for a sealed generation. |
| C7 | `manifestEpoch` and `retentionEpoch` are strictly monotonic. |
| C8 | The manifest holds `globalLastSeqFloor` (exact only for sealed generations); the anchor holds the live value. |
| C9 | A generation id is never reused, and a tombstoned generation's descriptor is never removed. |

## Rotation and the crash-cut contract

Rotation prepares the next generation's directory, empty journal and anchor
**first**, then switches the manifest pointer with one atomic rename. That
rename is the only commit point.

| Observed at open | Decision |
| --- | --- |
| Manifest names g-N, no g-N+1 directory | Reopen **g-N**; retry rotation |
| Manifest names g-N, g-N+1 exists and is **empty** | Reopen **g-N**; keep the orphan for an idempotent retry. Never delete it |
| Manifest names g-N, g-N+1 exists and is **non-empty** | **Poison** — unreachable before commit, so it means tampering |
| Manifest names g-N+1 | Reopen **g-N+1**; g-N is sealed and immutable |
| Journal extends past the anchor | Recompute the chain forward; adopt if it verifies, poison if a complete line fails |
| Trailing bytes with no newline | Trim exactly that run, record `recovery.torn_tail`. In a sealed generation this is poison |
| `manifest.json` absent, `.tmp` present | **Poison** — a temporary is never promoted |
| No manifest but generation directories exist | **Poison**, unless an authenticated bootstrap marker covers them (see Migration) |
| Intent with no outcome | Append an `uncertain` outcome with `host_restart_interrupted`. Never fabricate success, never auto-redispatch |

Nothing is ranked by mtime, file size, or highest sequence number: the manifest
is the sole authority for which generation is active.

## Producer wiring

Producers keep their existing call shape. `orchestration/audit_bridge.rs` is the
one place that translates `AuditEntry` into an authenticated record, and it is
where the v1 ledger's privacy defects are closed:

| v1 field | v2 record | Why |
| --- | --- | --- |
| `tool` | `op` | already a closed set |
| `outcome` | `outcome` | mapped to `accepted`/`rejected`/`uncertain`; anything unrecognised becomes **uncertain**, never `accepted` |
| `error_code` | `reason` + `code` | `reason` is the closed vocabulary; `code` keeps the exact string, constrained to `[a-z0-9_]{1,64}` so it cannot carry a path or a secret |
| `workspace` | `scope` | a real filesystem path becomes an opaque keyed digest |
| `request_id` | `request` | keyed digest |
| `session_id` | `actor` | keyed digest |
| `intent_id` | `producer` | keyed digest of the durable intent identity |
| `detail` | *(dropped)* | free text; on the rejected path it carried `OrchError::message`, which can contain paths and IO strings. It still reaches the local process log |

### Producer intent identity

`AuditEntry::intent_id` carries the identity of the durable lifecycle an entry
belongs to; when absent it falls back to the request id and then the session id,
so sites that already carry a request id need no change. An `Intent`-phase entry
opens that identity and only an `Outcome` carrying **the same** identity closes
it. An intent with no identity is tracked under its own sequence and can only be
closed by recovery, as uncertain — which is the honest outcome for an intent
nothing ever answered.

The anchor tracks up to `MAX_TRACKED_INTENTS` (256) open identities. Beyond that
the entries are still recorded and only the correlation set is bounded; the
anchor says so with `intentTrackingOverflowed`.

## Key custody

`AuditKeyCustody` covers the three deployment modes, and every one of them fails
closed rather than degrading to an unauthenticated ledger:

| Mode | Custody | Absent or unsafe material |
| --- | --- | --- |
| Packaged desktop | `Provided` (keychain bytes passed in by the shell) | store refuses to open |
| Headless service | `Environment { var }`, 64 hex characters | store refuses to open |
| External consumer | `Provided` (caller's own bytes) | store refuses to open |
| Default / tests | `LocalFile`, created on first use at mode `0600` | unsafe owner, mode, or link count refuses to open |

Errors carry only a stable code (`key_unavailable`, `manifest_mac_mismatch`).
No key path and no key bytes appear in any error, receipt, or health field.

## Cross-process exclusion

The manifest compare-and-swap is load, compare, atomic-rename — three steps,
not one — so an in-process mutex alone left a window where two handles on one
root could each read epoch N and each commit N+1. Every structural transaction
now also holds an advisory exclusive lock on `structural.lock` in the ledger
root, taken *between* the in-process mutex and the inner state lock so a waiter
never holds `inner` while it blocks.

`open` takes the same lock across recovery. Recovery is a mutating pass — it
trims torn tails, resumes authorized removals, rewrites anchors and appends
records — and running it unlocked let a ledger opened mid-rotation observe a
generation directory the manifest did not yet name, or a journal still being
written. That was the "raw ledger construction bypasses store locking" hole.

This is exclusion between *ledger handles*, which is what the manifest CAS
needed. It does not replace the `OrchStore` home lock.

## Concurrency

Three separate mechanisms, because they defend against three different things.

**Per-append atomicity.** The inner state lock is held across a whole append.
Releasing it between the journal write and the anchor update let a second
in-process appender read a stale tail and be issued a sequence that had already
been written.

**The structural barrier.** Rotation, retention, and the copy phase of an export
run inside one ledger-wide transaction that holds both the structural lock and
the inner lock for their whole extent. A rotation that released the inner lock
between its journal snapshot and its manifest commit let a concurrent append
land in the generation being sealed — stranding that entry outside the sealed
range and duplicating its sequence as the next generation's `firstSeq`. A
retention that built its manifest from a snapshot taken before its verification
could overwrite a rotation that committed in between, dropping a committed
generation and regressing the epoch.

**Manifest compare-and-swap.** Every structural commit re-reads the on-disk
manifest and requires its epoch to match the one the transaction observed. A
second *process* that committed underneath us fails the swap with
`concurrent_writer` instead of being silently overwritten.

Beyond that the ledger relies on the process-wide `InstanceLock`
(`src/instance_lock.rs`) and the orchestration store lock rather than taking a
third lock of its own, and it *detects* the violation it cannot prevent: before
each append it checks that the durable anchor still matches the in-memory tail.

## Retention

The only path that deletes bytes. It requires a **sealed, non-current**
generation, a completed verification of the target, and one of exactly two
bases — there is no third option and no default:

| Basis | What it proves | What it needs |
| --- | --- | --- |
| `RetentionRequest::exported_under(gen, seal_id)` | a verified export already carried these exact bytes | a seal **this ledger issued and re-verified**, found in `manifest.seals`, whose `carried` entry matches the generation's `firstSeq`, `lastSeq`, `finalTag`, `journalSha256` and `entryCount` as re-established inside the retention transaction |
| `RetentionRequest::under_grant(gen, grant)` | an operator accepted destroying the last copy | a verified single-use [capability grant](#capability-authority) for `RetainUnexported`, bound to that generation |

A caller-supplied seal id is a **lookup key, never a claim**. An unknown id is
`export_seal_unknown`; an id whose export withheld the range, holed it, or
carried a shorter prefix of it is `export_seal_does_not_cover`. A public export
withholds imported v1 bytes, so its seal records nothing about them at all and
cannot authorize deleting them.

The target is verified completely first — you may not tombstone evidence you
cannot currently vouch for.

`T1 verify → T2 intent → T3 commit tombstone → T4 remove bytes → T5 mark removed → T6 outcome`

### The caller always learns which side of T3 it is on

`retain` returns `Result<RetentionReceipt, RetentionFailure>`, and the failure
carries a **phase**:

| Phase | Meaning |
| --- | --- |
| `not_committed` | The tombstone was never committed. The generation is untouched; a retry is a fresh attempt. |
| `committed` | The tombstone is committed and the bytes are gone. |
| `uncertain` | The tombstone is committed; whether the bytes are gone is unknown. The deletion is authorized and permanent either way. |

A bare `Err` on either side of T3 looks identical to a caller, which is exactly
the thing an audit authority must not do: "nothing was deleted" and "the
deletion is committed and may be half applied" are different facts that demand
different responses. Every failure after the T3 commit is `uncertain`, never a
plain error a caller might read as "no effect".

Recovery converges from **both** sides of T4. Resume work is selected on
`removedAt` alone, not on the generation directory still existing: a crash
between removing the bytes and recording that removal leaves the directory gone
and `removedAt` unset, and keying off the directory skipped exactly that case —
so the tombstone read "committed but not removed" forever, a false statement
about a completed deletion that `uncertain` could never resolve out of.

T3 is the commit. A crash after it leaves a committed tombstone with the bytes
still present; the next open resumes the removal, which is the only authorized
deletion path. The tombstone keeps `firstSeq`, `lastSeq`, `chainBase` and
`finalTag` permanently, so the chain stays verifiable *across* the hole:
deleted history is provably deleted by an authorized transaction at a named
retention epoch, never merely missing.

Never: delete before T3, delete the active generation, delete over a torn
chain, delete on a read/rotation/export path, or delete to make room.

## Export

### Scope: public versus privileged raw

Imported v1 bytes are preserved **verbatim**, which means they still carry
whatever the v1 ledger recorded — raw workspace paths, free-text `detail`
holding `OrchError::message`, IO strings, and provider material. None of that
was ever redacted to the v2 rules, so it must not leave the machine in an
artifact anyone treats as public.

| Scope | Imported v1 generations | Declares |
| --- | --- | --- |
| `Public` (default) | **withheld** — named in `coverage` as `kind: "withheld"` with `withheldReason: "unauthenticated_legacy"`, and no files carried | `complete: false` |
| `PrivilegedRaw` | carried verbatim, for operator custody only | `containsUnauthenticatedLegacy: true` |

The verifier enforces the separation in both directions: a public export that
carries an unauthenticated generation is refused, and so is a raw export that
claims to withhold one. Relabelling one as the other fails the seal MAC.

`OrchStore::export_audit` is the public scope. `export_audit_privileged_raw`
requires a verified single-use grant for `PrivilegedRawExport`: naming a
different scope is not authority, the grant is. The grant is spent **before a
single byte is copied**, so a crash leaves the grant spent and no export rather
than a live grant beside a written privileged export.

### Format

`ExportFormat::Auto` emits v1 for a never-rotated, fully authenticated ledger
and v2 otherwise. `ExportFormat::V1` is **refused** for anything a v1 document
cannot represent — more than one generation, any tombstone, or any imported
generation — because a v1 document has no way to say "partial" or
"unauthenticated origin".

A v2 export carries a `coverage` array that must tile
`globalFirstSeq..globalLastSeq` exactly, with `kind: "hole"` elements for
retained ranges, and `complete` is true only when every element is a
generation. The chain must stitch across holes as well as generations. After
writing, the export is reopened by a fresh reader that shares no state with the
live ledger and re-verified before a path-free receipt is returned.

`verify_export` accepts both v1 and v2, so exports taken before generations
existed stay verifiable.

### What a verifier checks

`verify_export` re-derives everything from the sealed export manifest: its MAC,
that coverage tiles `globalFirstSeq..globalLastSeq` exactly, that the chain
stitches across holes, each carried journal's digest and length, and a full
chain scan of each carried generation. For a v2 export it also authenticates
the **copied ledger manifest** and cross-checks its installation, key,
manifest epoch, retention epoch and first sequence against the seal — that file
was previously carried unauthenticated, so a substituted one could misdescribe
generations, tombstones and retention to anyone who trusted the directory as a
whole.

### What an export commits

Export never rotates, truncates, deletes, or changes a journal byte, a
tombstone or a chain tag. It does commit two **additive** facts to the manifest:

- the **seal** it issued, recorded only after an independent reader verified
  the written copy from disk. Without this registry, "this range was already
  exported" would be an unverifiable claim made by whoever wanted the range
  deleted. Only ranges the export actually *carried* are recorded — never a
  withheld or holed element.
- for a privileged raw export, the **grant** it spent.

A failed `record_seal` removes the destination, so an export that returns an
error never leaves a directory behind whose seal id would look like retention
authority it does not carry. A crash between the verified copy and the seal
commit simply leaves no seal: the range stays undeletable until it is exported
again, which is the safe direction.

The registry keeps the 64 most recent seals. Forgetting one only ever makes
retention *more* conservative.

## Capability authority

Two operations are not ordinary ledger use: a **privileged raw export**, which
carries unredacted legacy bytes, and **retention of a generation no verified
export ever carried**, which destroys the last copy of a range. Before this
existed, the first was reachable by naming a different scope and the second by
setting a bare `allow_unexported` bool — so the authority for a deletion was
the deletion's own request.

Both now require an `AuthorityGrant`, which is:

- **authenticated** under its own key domain (`grokptah-audit.v2/authority`),
  so a captured seal, anchor or chain tag can never be replayed as authority;
- **bound** to one capability, one keyed subject digest, one installation and
  one key id;
- **expiring** — 300 s, and a grant claiming a longer life is rejected even if
  it verifies;
- **single use** — the spent `grantId` is recorded in the same manifest write
  that commits the effect, so there is no window in which a grant is spent
  without the effect, or the effect happened with the grant still spendable;
- **journaled** — issuing *and refusing* a grant both append a chained record,
  so a denied attempt to delete unexported history is as visible as a granted
  one.

Grants come from an `AuditAuthorityProvider`. The default is `DeniedAuthority`:
**a host that installs no provider can do neither operation at all.** The host
installs `LocalOperatorAuthority` only when `GROKPTAH_AUDIT_OPERATOR` names
capabilities explicitly (`privileged_raw_export`, `retain_unexported`, comma
separated), so a host that needs raw preservation exports does not thereby gain
the ability to delete history. An unrecognised value grants nothing.

> **This is a structural and evidentiary boundary, not an authenticated
> principal boundary.** There is no principal authority in the codebase yet
> (#460/#461), so the only source a shipped build can assert is
> `AuthoritySource::LocalOperator` — an operator act on the host, not a
> verified identity. Every grant and every tombstone written under one records
> that source permanently, so an operator-asserted deletion can never later be
> read as a principal-authorized one. When #460/#461 land they supply a
> provider that returns `AuthoritySource::Principal`; nothing else changes.

## Accepted-but-not-journaled entries

`OrchStore::append_audit` returns only after a durable append.
`OrchStore::enqueue_audit` hands the entry to a writer thread and returns
before it is journaled — that is the point of the queue. "Accepted" therefore
cannot mean "durable", and it no longer pretends to: it means **the entry's
loss would be visible**.

A durable, authenticated `pending.json` marker is written and fsynced *before*
`enqueue_audit` returns, on the transition out of idle, and removed once the
queue drains through a durable append. One marker covers a whole burst, so a
busy writer pays for it once.

Finding a marker at open means a crash happened with entries in flight. Between
zero and `AUDIT_IN_FLIGHT_BOUND` of them are gone and **nothing on disk can
narrow that**, so recovery records the bound (`maxLostEntries`) rather than a
number that would read as certainty, journals it under
`reason: "accepted_not_journaled"` with an uncertain outcome, and only then
clears the marker. A crash in that window re-reports the same uncertainty,
which over-states doubt; clearing first would lose the evidence entirely.
`get_capacity` surfaces it as `acceptedNotJournaledEpisodes` and
`maxAcceptedNotJournaled`.

The bound is the channel capacity **plus one**. The writer `recv`s an entry —
freeing its channel slot — before the append that makes it durable, so a crash
at that instant leaves a full channel *and* the entry in the writer's hand.
Reporting the capacity alone under-counted by exactly one, and an under-count
of a loss bound is the direction that hides evidence.

### Fencing structural work against the queue

The marker makes a *crash* loss visible; it does nothing for a structural
transaction taken right now. Export and rotation therefore refuse with
`accepted_work_in_flight` while any accepted entry is unjournaled:

- an **export** would otherwise seal a range and call it `complete` while
  entries destined for it were still queued;
- a **rotation** would otherwise strand them — the writer would append them to
  the *next* generation, so the sealed range would silently omit work the
  caller was already told was accepted.

Lock order is `inner` before `pending` everywhere, which is what lets a
structural transaction read the in-flight count at all. Holding `inner` across
the whole of `note_accepted` also means a running transaction blocks new
acceptances, so the count it reads cannot go stale underneath it.

## Counter exhaustion

Sequence, manifest epoch, retention epoch and generation index all fail closed
at `u64::MAX`/`u32::MAX` with `sequence_exhausted` rather than saturating.
A saturated sequence would reissue one authenticated position to two different
entries, and a saturated manifest epoch would stop being a compare-and-swap,
so both writers could believe they won.

## Migration from v1

On the first open of a root that has never committed a manifest, an optional
`legacy_v1_dir` is imported: `audit.jsonl.1` (older) then `audit.jsonl`. The
bytes are copied **verbatim** and the generations are labelled:

- `originAuthenticated: false` — preserved, never vouched for;
- `sequenceOrigin: import_assigned` — the sequences were assigned at import;
- `precedingLossUnknown: true` on the oldest, because v1 already destroyed
  anything older without a record.

Those bytes cannot be retroactively authenticated and nothing claims they are.
The *boundary* is authenticated: each imported generation's `finalTag` is an
HMAC over its exact SHA-256, so the first native generation still chains from a
real tag.

An import declares its staged directories in an authenticated `bootstrap.json`
before creating them. A crash before the manifest commit is recovered by
clearing exactly those declared directories and re-running; the v1 source files
are read-only inputs and are never moved or truncated, so nothing can be lost.

## Limits this does not close

- **Producer intent/outcome pairing is partial.** Most of the 33 shipped
  producers record outcomes only; run finalization and retention are the real
  intent/outcome pairs. `actor` is the session UUID, not an authenticated
  principal, and `authzRev`/`capRev`/`policyRev` are unset. Completing this
  needs the canonical auth, principal, queue and Computer Use authorities
  (#460, #461, #458) and is deliberately not guessed at here.
- **Computer Use does not reach this ledger.** It keeps its own bounded per-run
  ring in `computer_use/types.rs`.
- **Production key custody is not wired.** `AuditKeyCustody` has the packaged
  desktop, headless service and external consumer variants, and each fails
  closed, but the shipped default is still `LocalFile`: no caller passes
  keychain or environment material yet. There is also no versioned key rotation
  — a changed installation key is *rejected*, not rolled over.
- **Capability authority has no authenticated principal behind it.** Grants
  are verified, single-use, subject-bound, expiring and journaled, and the
  default provider grants nothing — but `AuthoritySource::LocalOperator` is a
  process-level operator assertion, not a proven identity. Anything that can
  set `GROKPTAH_AUDIT_OPERATOR` in the host's environment can assert it.
  Closing this needs #460/#461; the source is recorded permanently so the
  distinction is never lost.
- **The raw `AuditLedger` API is reachable in-process.** Any code inside the
  bridge that holds the ledger handle can call `issue_authority`. The boundary
  is structural and evidentiary — nothing happens without a chained record —
  not a sandbox.
- **Queued entries are bounded-uncertain, not durable.** `enqueue_audit`
  guarantees that loss is *visible*, not that it does not happen. Call sites
  needing a durable append use `append_audit`.

- **Joint rollback.** Restoring the manifest, every anchor and every journal to
  the same earlier moment satisfies every invariant above, and the ledger opens
  clean. Detecting it needs a platform monotonic counter or a remote witness.
  `witness.rs` defines only the seam; **no witness service is implemented**, the
  default is `UnwitnessedBoundary`, and every export receipt states its
  `witnessState`. A witness that cannot be reached is fail-soft for operation
  and never upgrades into an implied guarantee.
- **Boot verification is two-tier, not absent.** The active generation's tail
  beyond the anchor is fully chain-verified at every open. Every sealed,
  non-tombstoned generation is checked at open against the byte length and
  SHA-256 the authenticated manifest recorded for it, so tampering with sealed
  history fails the open rather than surviving until someone happens to export
  or retain. That is a streaming digest per sealed journal, not a full chain
  replay: `verify_all` remains the per-entry HMAC check, and export and
  retention still run it on the ranges they touch.
- **Key loss.** Losing a retired chain key makes old generations unverifiable.
  It does not delete them, and the ledger reports "unverifiable" rather than
  anything softer.
- **Filesystem access.** An operator who can delete the audit directory can
  still do so. Only an external append-only sink or a witness makes that
  detectable.
- **Async producers are not causally ordered against synchronous ones.** A
  queued entry can still be journaled after a synchronous shutdown, recovery
  or retention record that was issued later in real time. The pending marker
  makes *loss* visible; it does not impose an ordering. Export and rotation can
  likewise snapshot while entries are in flight.
- **Interrupted intents are closed in aggregate.** Recovery states the exact
  number of intents left open and marks the outcome uncertain. Per-intent
  correlation needs producer-supplied intent ids and is not implemented here.
- **A poisoned audit blocks startup.** `OrchStore::open` fails when the ledger
  cannot be authenticated. There is no automatic destructive repair: the
  operator exports and inspects, and any decision to move past a poisoned
  ledger is theirs and is itself recorded. This is an availability cost taken
  deliberately.
- **Free-text `detail` is no longer durable.** `(op, outcome, code, reason)`
  identifies every event; the human message stays in the local process log.
- **Computer Use keeps its own bounded per-run ring** in
  `computer_use/types.rs`. It does not write to this ledger, and wiring it is
  out of scope here.
- **Host lifecycle (#455) is untouched.** The same-home reuse test exercises the
  `OrchStore` lock through the production lifecycle only; the host `InstanceLock`
  seam is a separate dependency and nothing here masks it.
