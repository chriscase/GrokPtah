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

The only path that deletes bytes. It requires a **sealed, non-current** generation,
an operator authorization, and either an export seal covering the range or an
explicit `allow_unexported` override that is itself recorded permanently. The
target is verified completely first — you may not tombstone evidence you cannot
currently vouch for.

`T1 verify → T2 intent → T3 commit tombstone → T4 remove bytes → T5 mark removed → T6 outcome`

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

`OrchStore::export_audit` is the public scope; `export_audit_privileged_raw` is
the other, and its name is the warning.

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

Export never rotates, truncates, deletes, or advances the manifest epoch.

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
- **Retention authority is unauthenticated.** `allow_unexported` is an argument,
  not a proven operator identity.

- **Joint rollback.** Restoring the manifest, every anchor and every journal to
  the same earlier moment satisfies every invariant above, and the ledger opens
  clean. Detecting it needs a platform monotonic counter or a remote witness.
  `witness.rs` defines only the seam; **no witness service is implemented**, the
  default is `UnwitnessedBoundary`, and every export receipt states its
  `witnessState`. A witness that cannot be reached is fail-soft for operation
  and never upgrades into an implied guarantee.
- **Boot verification is bounded.** The active generation's tail beyond the
  anchor is always verified. Sealed generations are verified on export, on
  retention, and on explicit `verify_all`, not at every start — so tampering
  with a sealed generation is detected at those points, not necessarily at the
  next boot.
- **Key loss.** Losing a retired chain key makes old generations unverifiable.
  It does not delete them, and the ledger reports "unverifiable" rather than
  anything softer.
- **Filesystem access.** An operator who can delete the audit directory can
  still do so. Only an external append-only sink or a witness makes that
  detectable.
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
