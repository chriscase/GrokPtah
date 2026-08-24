# Build and CI performance

GrokPtah keeps an uncached Cargo and npm path for proofs and for CI that
unsets `RUSTC_WRAPPER`. **Local macOS operator builds** require
`RUSTC_WRAPPER=sccache` against a stable shared cache **after `sccache` is
verified on PATH**. Cached objects never replace formatting, lint, test, or
packaging commands.

The local target policy is **repository-family reuse outside checkouts**, not
a private `target/` under every worktree. Per-worktree and `/private/tmp`
multi-GB targets are not the default. See [Authoritative local build-cache
policy](#authoritative-local-build-cache-policy).

## Measured baseline

The baseline was recorded on 2026-07-29 at commit `323a5be` on an Apple
Silicon Mac17,2 with 10 CPUs, 24 GiB RAM, Rust 1.92.0, Node 25.2.1, and npm
11.6.2. Cold Rust runs used `CARGO_INCREMENTAL=0` with `RUSTC_WRAPPER` unset.

| Workload | Time |
| --- | ---: |
| `npm ci` | 4.50 s |
| Frontend typecheck and 94 tests | 12.99 s |
| Desktop `cargo check && cargo test`, cold | 225.13 s |
| Same desktop command, no source changes | 7.21 s |
| Focused bridge test after touching `src/textutil.rs` | 14.01 s |
| Bridge format, clippy, tests, and oracle rerun, cold | 197.84 s |
| Frontend production build | 3.28 s |

The hosted PR job at the same baseline took 4m49s. A duplicate push job for
the exact commit ran concurrently and took 5m48s. Queue delay on recent runs
was 0-11 seconds, so compilation and duplicate execution were the useful
targets. No self-hosted runners are configured.

An empty, 2 GiB local `sccache` compiled the bridge test targets in 65.34
seconds and stored 120 MiB. After removing only that test target, the same
workload took 18.55 seconds with 165 of 165 cacheable compilations served
from cache, a reproduced 71.6% reduction for that compile-only workload.

Rust objects did not hit when the source worktree and target path both
changed. That measurement is why compatible sequential lanes now reuse one
**stable repository-family `CARGO_TARGET_DIR` outside checkouts**, not a
private target under each worktree. Concurrent or incompatible builds still
must not share a writable target; see the policy below.

## Implementation results

After replacing the redundant desktop `cargo check && cargo test` pair with
locked `cargo test`, an equivalent cold, uncached target completed in 77.77
seconds. All six desktop Rust tests passed. Compared with the 225.13-second
baseline, this removed 147.36 seconds, or 65.5%, from that local stage.

Adversarial cache checks used disposable targets and retained command logs:

- Changing compiler flags caused 141 Rust misses and no cache errors, proving
  incompatible objects were not reused.
- A deliberately interrupted compile recovered successfully in 21.62
  seconds; the partial cache reported no read or write errors.
- Two simultaneous compile clients, each with a **private** target, completed in
  61.74 and 60.75 seconds against one cache with no corruption or cache
  errors. Different target paths again produced Rust misses. Concurrent
  lanes still isolate; compatible **sequential** lanes now reuse one family
  target so those misses are not the default.
- The 2 GiB configured maximum was reported by `sccache`. Disposable targets
  and caches were removed only after exact-path checks, reclaiming about 25
  GiB while preserving the logs. Targets still in use were not deleted
  (current cleanup still refuses active, protected, and shared-family paths).

Hosted before/after timing and GitHub cache size are recorded from the exact
pull-request head because the local disk backend is not equivalent to
GitHub's cache service.

The first hosted run on PR #203 was an empty-cache proof. It passed in 5m01s:
desktop Rust took 2m17s, bridge verification took 1m59s, and `sccache`
reported 84 hits, 573 misses, and zero errors. This is 12 seconds slower than
the earlier single PR job, but it replaced the two simultaneous PR and push
jobs that had consumed about 10.6 runner-minutes for one commit.

A same-commit rerun produced 653 hits, 2 misses, a 99.69% hit rate, and zero
cache errors. Desktop Rust fell to 1m39s, and bridge compilation and tests
reached a pre-existing parallel store-lock race in 1m03s versus 1m59s cold.
CI now uses the repository's documented single-threaded bridge test mode to
avoid that race without skipping any tests. The resulting GitHub compiler
cache occupied 412,458,826 bytes across 576 content-addressed entries; the
existing npm cache occupied 29,930,510 bytes.

## Authoritative local build-cache policy

This section supersedes the earlier “each worktree keeps a private Cargo
target” / “`sccache` is optional” operator rule. It is the GrokPtah local
macOS policy after reclaiming on the order of ~90 GiB of stray per-worktree
and `/private/tmp` targets. GitHub Actions cache behavior is unchanged
below; hosted runners do not use `~/Library/Caches/grokptah`.

**Build artifacts are disposable. Source and commits are deliverables.**

### 1. Required `sccache` (after verify)

```sh
command -v sccache
sccache --show-stats
export SCCACHE_DIR="$HOME/Library/Caches/grokptah/sccache"
export SCCACHE_CACHE_SIZE=2G
export RUSTC_WRAPPER=sccache
export CARGO_INCREMENTAL=0
mkdir -p -- "$SCCACHE_DIR"
```

Do not export `RUSTC_WRAPPER=sccache` until `sccache` is on PATH and
`--show-stats` (or an equivalent start) succeeds. The canonical cache is
`~/Library/Caches/grokptah/sccache`. An older document used
`~/Library/Caches/GrokPtah/sccache`; on case-insensitive APFS those paths
may alias the same directory. New handoffs name the lowercase `grokptah`
path.

Uncached proof (not the default operator path):

```sh
env -u RUSTC_WRAPPER -u SCCACHE_DIR CARGO_INCREMENTAL=0 cargo test --locked
```

### 2. Repository-family `CARGO_TARGET_DIR` (compatible, non-concurrent)

Compatible, **non-concurrent** lanes reuse **one** stable repository-family
target **outside checkouts**:

```text
~/Library/Caches/grokptah/cargo-target/<family-key>/
```

Fence `<family-key>` so a hit is only reused when all of these match:

- rustc/cargo toolchain (`rust-toolchain.toml` / `rustc -vV`)
- rustc target triple
- cargo features and profile (dev/release, and the exact feature set)
- lock/dependency graph (the workspace `Cargo.lock` identity in use)

Nested workspaces in this repo (desktop `src-tauri`,
`crates/codegen/grokptah-agent-bridge`, generated root workspace) are
**different families** when their lockfiles or profiles differ. Do not point
them at one writable target.

Example:

```sh
export CARGO_TARGET_DIR="$HOME/Library/Caches/grokptah/cargo-target/${FAMILY_KEY}"
mkdir -p -- "$CARGO_TARGET_DIR"
```

### 3. Never concurrently share a writable target

Two `cargo`/`rustc` processes must not share one writable `CARGO_TARGET_DIR`.
Truly **concurrent** or **incompatible** (toolchain, triple, features/profile,
or lock/dependency graph) builds get an **exact isolated target only for that
lane**:

```text
~/Library/Caches/grokptah/cargo-target/isolated/<lane-id>/
```

Remove that isolated target when the lane is inactive, after the cleanup
gates below. Do not leave isolated targets as a second default family.

### 4. Forbidden default locations

Never put multi-GB Cargo targets under:

- `/private/tmp` or `/tmp`
- the review/worktree checkout (`<worktree>/target`) **by default**

A worktree-local `target/` is not the operator default. Isolated-lane
directories still live under `~/Library/Caches/grokptah/cargo-target/isolated/`,
not under `/private/tmp`.

### 5. Cleanup gates (refuse by default)

Before deleting a target or sccache directory:

1. Record the **exact** path.
2. Record **size** (`du -sh -- "$path"`).
3. Record **owner**.
4. Confirm **no `cargo` or `rustc` process** is using it.
5. Confirm **no open handles** (`lsof` on that path).

**Refuse** deletion when the path is active, protected, or a live
shared-family target. Do not `rm -rf` a guessed `target`, `$TMPDIR`, or
another project’s cache. Isolated inactive lanes may be removed only after
those checks pass.

Reset only the documented GrokPtah sccache path (equality check is
intentional):

```sh
expected="$HOME/Library/Caches/grokptah/sccache"
test "${SCCACHE_DIR:-}" = "$expected"
sccache --stop-server
# still refuse if cargo/rustc is running or the directory has open handles
rm -rf -- "$expected"
```

Do not substitute an unresolved, empty, home, or shared-unrelated cache
path.

### 6. Handoffs and Stage 11 drill evidence

Handoffs record `CARGO_TARGET_DIR`, `SCCACHE_DIR`, owner, and reason
(`sequential-family-reuse`, `concurrent-isolation`, or
`incompatible-isolation`).

Stage 11 drill evidence must cover all of:

- **compatible sequential reuse** — a second non-concurrent compatible lane
  reuses the family target
- **concurrent forced isolation** — overlapping builds get distinct isolated
  targets; no shared writable target
- **incompatible forced isolation** — toolchain/target/features/profile/lock
  mismatch does not join the family directory
- **crash cleanup** — an inactive isolated target is removed only after the
  cleanup gates
- **active-target deletion refusal** — cleanup refuses a live family or
  in-use isolated path

Useful focused commands (inherit the verified `RUSTC_WRAPPER` / family
`CARGO_TARGET_DIR`):

```sh
cd crates/codegen/grokptah-agent-bridge
cargo test --locked --lib textutil
cargo test --locked --test bridge_lifecycle -- --test-threads=1

cd ../../../../desktop/src-tauri
cargo test --locked
```

## Diagnostics

Inspect cache effectiveness and disk pressure:

```sh
sccache --show-stats
du -sh -- "${SCCACHE_DIR:?SCCACHE_DIR is not set}"
du -sh -- "${CARGO_TARGET_DIR:?CARGO_TARGET_DIR is not set}"
df -h "$HOME/Library/Caches/grokptah"
```

## GitHub Actions cache

The Desktop workflow pins `sccache` 0.16.0 and namespaces cached compiler
objects by operating system, architecture, Rust toolchain, both nested
workspace lockfiles, and a manual schema version. Compiler inputs, flags,
features, and build mode are also part of `sccache` object keys.

GitHub's repository cache quota and eviction policy bound hosted storage.
Fork pull requests are explicitly read-only. Cache data is treated as
untrusted: every required command still runs, incompatible objects miss
harmlessly, and cache service errors fall back to ordinary compilation.

The workflow does not cache Cargo target directories, release bundles,
credentials, keychain or signing state, user data, or artifacts shared with
another repository. Change the `grokptah-v1` namespace in the workflow to
invalidate all hosted compiler-cache entries.

## Git and worktree hygiene

Feature branches run the complete Desktop workflow through their pull
request. Push-triggered runs are reserved for `main`, preventing the same
feature commit from consuming a second macOS runner. New commits cancel an
older run for the same pull request; `main` runs are never canceled.

Before removing a worktree, verify it is clean, attached to the expected
branch, and recoverable from a merged commit or remote branch:

```sh
git worktree list
git -C /absolute/worktree/path status --short --branch
git branch --contains <commit>
git branch -r --contains <commit>
```

Do not remove dirty, active, detached-unverified, or unmerged worktrees.
Issue-specific commits can be promoted together through one integration PR
without squashing away their attribution. Rebase the integration head once,
immediately before the final gate, and run hosted CI on that exact head.

## Known baseline limits

- A root-wide `cargo test --no-run` on the baseline fails in
  `xai-grok-shell-base` because `EnvVarGuard` is test-gated in a dependency.
  The supported verification path remains the focused desktop and bridge
  workspaces documented above.
- The unsigned release attempt built the optimized executable and `.app`,
  then failed in the macOS DMG script after 230.22 seconds. Release
  packaging is not cached and needs a separate reliability fix before its
  timing can be optimized.
- The repository has no release workflow or self-hosted runners. If
  self-hosting is introduced, use repository-scoped cache namespaces,
  isolated job workspaces, bounded disk cleanup, and no cross-repository
  access to private compiler objects.
