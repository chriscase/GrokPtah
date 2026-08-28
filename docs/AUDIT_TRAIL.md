# Durable audit generations (`grokptah-audit.v2`)

`crates/codegen/grokptah-agent-bridge/src/audit/` implements the durable,
append-only, tamper-evident audit authority used by long-running agents and
embeddable consumers (#443).

This document describes the format, the crash-cut contract, and — just as
importantly — the limits it does **not** close.

## Why it exists

The shipped orchestration store now uses this authority for every
`append_audit`/`enqueue_audit` call. The old v1 files may remain as immutable
migration inputs, but no runtime path appends to them and there is no second
active audit ledger. Before this integration, v1 rotation appended to
`audit/audit.jsonl` and, at 4 MiB, deleted `audit.jsonl.1`; three consequences
followed:

1. The third-oldest generation is destroyed with no manifest, tombstone, marker
   or audit record. Nothing anywhere states that it existed.
2. A crash between the delete and the rename loses the previous generation
   while the current one survives, so the surviving state looks correct.
3. A crash between the rename and the first append leaves no `audit.jsonl`, so
   "rotated a moment ago" and "never audited anything" are indistinguishable.

Those v1 entries carried no sequence or MAC, and a dropped entry was only an
in-memory error. The migration preserves that history but never upgrades its
origin into an authenticated claim.

## Layout

```
<root>/
  manifest.json          authenticated; names the active generation. THE pointer.
  manifest.json.tmp      transient only; a reader never promotes it
  gap.json               authenticated durable dropped-entry evidence
  export-seals.json      authenticated export authorization index
  .audit-key             private installation custody key (store parent)
  .audit-key-epochs/     private retired/current epoch material
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
| Export seal index | `grokptah-audit-seals.v2` | `K_seal` | after a verified export |
| Export | `grokptah-audit-export.{v1,v2}` | `K_seal` | never (a copy) |
| Journal line | `v: 2` records | `K_chain` | append-only |

All are `deny_unknown_fields` and carry a MAC over the canonical bytes of
themselves minus that MAC. Canonical JSON sorts object keys explicitly and
rejects non-integer numbers, so a MAC never depends on `serde_json` feature
unification elsewhere in the dependency graph.

Keys are derived from one installation key and an authenticated custody epoch
by domain-separated HMAC (`chain`, `manifest`, `anchor`, `seal`, `actor`).
Retired epochs stay in a private key ring so old generations remain verifiable.
Packaged desktop and headless service modes require a private owner/mode/link
count checked file; external consumers must inject held key material. There is
no environment-variable, provider-credential, path disclosure, or unsafe-mode
fallback.

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

## Single-writer discipline

The orchestration store's process-wide `.store.lock` remains the ownership
boundary, and the v2 ledger serializes append/rotation/retention transactions
inside that owner. Before each append it checks the durable anchor's complete
state, and a second writer that advanced the anchor poisons with
`concurrent_writer` instead of interleaving chains. A standalone
`AuditLedger` has no implicit process lock; production callers must use the
shared `OrchStore` custody boundary.

## Retention

The only path that deletes bytes. It requires a **sealed, non-current,
authenticated** generation, an operator authorization, and either an export
seal covering the range or an explicit `allow_unexported` override that is
itself recorded permanently. The target is verified completely first — you may
not tombstone imported, unverified, or otherwise unverifiable evidence.

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
clearing exactly those declared directories and re-running; a crash after the
manifest commit clears only the now-redundant marker. The manifest atomic rename
is the single switch boundary. The v1 source files are read-only inputs and are
never moved or truncated, so nothing can be lost.

The store adapter maps producer operations to keyed intent identities and
outcomes. The same v2 authority is used for orchestration, provider attempts,
approvals, queue/background work, subagents, cancellation, shutdown, and
Computer Use service mutations. Free-form legacy `detail` is deliberately not
adapted; the bounded public projection contains no prompt, credential, path,
locator, clipboard, frame, HMAC key, or private provider payload.

## Operator recovery

1. Stop every owner of the orchestration home and take a byte-preserving copy
   before inspection. Use the explicit store shutdown boundary; do not delete a
   lock file to force a restart.
2. Reopen with the same custody mode and key ring. Inspect `audit_status`,
   then run `verify_audit` or a fresh export. A `poisoned`/unavailable result is
   an incident state, not a prompt to edit JSON.
3. Preserve `manifest.json`, generation directories, tombstones, gap evidence,
   legacy v1 inputs, and key epochs. Restore a trusted backup or supply the
   missing authorized key material; do not renumber, rewrite, reseal, or delete
   poisoned evidence.
4. Retention is the only deletion path and requires a fully verified,
   authenticated, non-current generation plus an authenticated export seal or
   explicit operator override. A committed tombstone may resume its already
   authorized byte removal after restart; this is convergence, not repair.

There is no automatic destructive repair, no silent gap filling, and no
rollback guarantee without a configured witness. Escalate uncertain provider,
queue, Computer Use, or shutdown outcomes for explicit reconciliation.

## Limits this does not close

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
- **Key loss.** Losing a retired epoch makes the affected generation
  unverifiable. It does not delete it, and the store fails closed rather than
  reporting a clean ledger.
- **Filesystem access.** An operator who can delete the audit directory can
  still do so. Only an external append-only sink or a witness makes that
  detectable.
- **Interrupted intents are closed in aggregate.** Recovery states the exact
  number of intents left open and marks the outcome uncertain. Producer-supplied
  request IDs receive exact keyed intent continuity; legacy records without one
  use a deliberately weaker session/tool fallback and make no stronger claim.
- **Shutdown ownership.** Explicit `OrchStore` drop releases its store lock and
  the two-process reuse gate proves this store boundary. Full host clone
  quiescence remains owned by the separate #455 lifecycle work; this change
  does not claim to implement or qualify that host runtime.
