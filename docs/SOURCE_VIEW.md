# Read-only source inspection (v1)

Contract id: `grokptah.source-view.v1`.
Schema: [`schemas/grokptah-source-view.v1.schema.json`](./schemas/grokptah-source-view.v1.schema.json).
Golden fixtures: [`schemas/grokptah-source-view.v1.fixtures.json`](./schemas/grokptah-source-view.v1.fixtures.json).

Clicking a file in GrokPtah reads it. It does not compose a prompt, it does not
spend a turn, and it does not return whatever a model chose to quote. This
document describes the one path that read follows and the points at which it
refuses.

## The single authority path

```text
  authorization context ─┐
                         ├─ issue ─→ RootSnapshot { one opaque token per root }
  candidate roots ───────┘                     │
                                               ▼
  token + acting context ─→ resolve ─→ ResolvedRoot (held-open directory)
                                               │
                    lexical containment ───────┤
                                               ▼
           handle-relative no-follow open ─→ OpenedDocument
                                               │
                     bounded chunk read ───────┤
                                               ▼
                                        SourceDocument
```

Every step is a refusal point and every refusal is one of the closed codes in
`SourceViewError::CODES`. There is no route to a byte that skips one, and no
route that picks a root on the caller's behalf.

### Snapshots are non-mutating

Issuing a snapshot reads live authorization state, observes each candidate
directory, and records the result in an in-process registry. Nothing durable
changes, so a viewer that crashes mid-snapshot leaves no trace, and a viewer
failure can never alter what a reviewer is allowed to promote.

### One request names exactly one root

A snapshot returns one opaque token per approved root, and a token is the only
way to name a root. A selector that matches several roots returns `ambiguous`
and the reader must choose; a selector that matches none returns `absent`.
Neither falls back to "the first workspace" — silently substituting one tree
for another is how a reviewer approves a change they never read.

### Authorization is checked at action time

The acting principal (`principalId`, `tenantId`, `projectId`, `sessionId`) and
a fingerprint of the policy inputs are recomputed on **every read** and
compared with the snapshot. A run discarded, a project closed, a permission
mode changed, or a different principal asking all refuse.

The desktop is single-tenant, so its tenant is the process instance and its
project is the open workspace. A hosted broker populates the same four fields
from its own directory; the contract does not change, only where the values
come from.

### Replay policy: `idempotent-within-validity`

A token is a bearer capability for one root in one snapshot. Reads are
non-mutating and idempotent, so replaying a token inside its validity window is
paging, not an attack. A replay is refused when the tag does not verify, the
snapshot is unknown or revoked, the deadline has passed, the principal or
policy fingerprint differs, or the root's on-disk identity has changed.
Expired snapshots are swept on every issue and resolve, and the registry is
capped, so an evicted token fails closed as `snapshot_unknown`.

## Containment

**Lexical.** `..` is refused outright rather than collapsed. UNC paths, the
Windows device namespaces (`\\?\`, `\\.\`), and drive-relative paths are
refused on every platform. Reserved device names, alternate data streams, and
names with trailing dots or spaces are refused under the Windows policy, which
tests force on everywhere so the table is exercised on every runner.

**Physical.** On Unix the root directory is opened once and held; every
component is resolved from that handle with `openat` under `O_NOFOLLOW`. No
path string is re-resolved, so a symlink swapped in mid-walk is refused by the
kernel rather than raced past.

**Identity.** The held root is re-checked before each read: same device and
inode, and still linked. The link-count check is what defeats delete-and-
recreate at the same path, which inode comparison alone does not — filesystems
reuse inodes, and in practice do so immediately.

### Platform differences, stated plainly

| | Unix | Windows |
| --- | --- | --- |
| Component resolution | `openat` from a held directory handle | opened by the path built so far |
| Link refusal | kernel `O_NOFOLLOW` | `FILE_FLAG_OPEN_REPARSE_POINT` + attribute check |
| TOCTOU | closed | check-then-use |
| Node identity | device + inode (`exact`) | creation/write time, size, attributes (`heuristic`) |
| Removal detection | link count reaches zero | not available; identity comparison only |

The standard library exposes no handle-relative open and no file index on
Windows, so the Windows walk is weaker. `IdentityStability` reports which of
the two a given document was vouched for by, and the viewer says so rather
than implying a guarantee it does not have.

## Bounded reads

* Chunks are bounded in bytes, lines, and line width. Caller limits are
  clamped, never widened; `0` means "use the default", never "unbounded".
* A chunk may end mid-line. It says so (`continuesNext`), the cursor carries
  the line number to resume (`continuesLine`), and the next chunk continues
  that line under the same number. `LineAssembler` in Rust and
  `appendSourceChunk` in TypeScript are the one implementation of that rule on
  each side.
* UTF-8 is decoded incrementally with a carry of at most three bytes, so a
  character split across a chunk boundary survives instead of becoming a
  replacement.
* Classification is honest: `completeScan` is false when the verdict describes
  only the scanned prefix, and `scannedBytes` says how much was examined.
* Identity is a BLAKE3-256 digest of the whole file within the digest budget,
  and a pinned handle identity above it. Which one is in force is reported.

## Promotion evidence

A per-file review is a convenience; the raw diff is the authority. When the
parse cannot account for the whole diff — because the review capped it, or
because lines could not be attributed to a file — the raw diff is shown as
received, the reasons are named, and acknowledgement is withheld. Promotion
requires a review, complete evidence, and an explicit acknowledgement, and is
guarded in the handler as well as disabled in the UI.

## Broker parity

The desktop reaches this boundary over Tauri IPC; a browser reaches an
equivalent boundary through an authenticated ContextDesk broker. Both
implement the same closed operation set and share request validation and
response parsing, so a contract change that breaks one breaks both.

| Route | Method | Notes |
| --- | --- | --- |
| `GET /bindings/{bindingId}/source-view/snapshot` | read-only | Issuing a snapshot changes nothing, so it is a `GET`. `sessionId` is an optional query parameter. |
| `POST /bindings/{bindingId}/source-view/read` | read-only | A `POST` because the request carries a token, a path, and a cursor that do not fit a URL — and because retrieving file content is worth the broker's CSRF protection. Requires an idempotency key. |
| `POST /bindings/{bindingId}/source-view/revoke` | mutating | Refuses every token derived from one snapshot. |

## What never crosses the boundary

Absolute paths and file content never appear in a descriptor, a receipt, or an
error. A tree is identified by `pathDigest`, a file by `identity`, and people
read a short two-segment label plus a digest prefix. The TypeScript parsers
refuse any payload carrying `path`, `absolutePath`, `rootPath`,
`workspacePath`, or `cwd`, and both Rust and TypeScript assert the absence in
test.
