# Build and CI performance

GrokPtah keeps the ordinary uncached Cargo and npm paths fully supported.
Compiler caching is an optional acceleration: a miss compiles normally, and
cached objects never replace formatting, lint, test, or packaging commands.

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
changed. Keep each worktree's Cargo target private; do not point concurrent
worktrees at a shared writable target directory.

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
- Two simultaneous compile clients, each with a private target, completed in
  61.74 and 60.75 seconds against one cache with no corruption or cache
  errors. Different target paths again produced Rust misses.
- The 2 GiB configured maximum was reported by `sccache`. Disposable targets
  and caches were removed only after exact-path checks, reclaiming about 25
  GiB while preserving the logs. Active worktree targets were not deleted.

Hosted before/after timing and GitHub cache size are recorded from the exact
pull-request head because the local disk backend is not equivalent to
GitHub's cache service.

## Optional local compiler cache

Install `sccache` separately, then enable it in a shell:

```sh
export SCCACHE_DIR="$HOME/Library/Caches/GrokPtah/sccache"
export SCCACHE_CACHE_SIZE=2G
export RUSTC_WRAPPER=sccache
export CARGO_INCREMENTAL=0
```

The cache is local to GrokPtah and bounded to 2 GiB. It may be reused by
GrokPtah worktrees, but a hit is not guaranteed when absolute source or
target paths differ. The build does not require `sccache`; omit these
variables when it is unavailable.

Useful focused commands:

```sh
cd crates/codegen/grokptah-agent-bridge
cargo test --locked --lib textutil
cargo test --locked --test bridge_lifecycle -- --test-threads=1

cd ../../../../desktop/src-tauri
cargo test --locked
```

For a deliberately uncached proof:

```sh
env -u RUSTC_WRAPPER -u SCCACHE_DIR CARGO_INCREMENTAL=0 cargo test --locked
```

## Diagnostics and reset

Inspect cache effectiveness and disk pressure:

```sh
sccache --show-stats
du -sh -- "${SCCACHE_DIR:?SCCACHE_DIR is not set}"
df -h .
```

Reset only the documented GrokPtah cache path:

```sh
expected="$HOME/Library/Caches/GrokPtah/sccache"
test "${SCCACHE_DIR:-}" = "$expected"
sccache --stop-server
rm -rf -- "$expected"
```

The equality check is intentional. Do not substitute an unresolved, empty,
home, or shared cache path. Cargo build outputs are separate; use `cargo
clean` from the intended workspace rather than deleting a guessed target.

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
