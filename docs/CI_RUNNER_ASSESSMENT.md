# Self-hosted runner assessment (#202, Wave 3)

**Date:** 2026-07-31 · **Status:** assessment only — no infrastructure provisioned
(`repos/chriscase/GrokPtah/actions/runners` → `total_count: 0`).

## Recommendation

**Do not adopt self-hosted runners for this repository as it stands.** The
blocking reason is security, not cost or speed, and it is not a tuning problem
that better configuration solves.

## Governing constraint: this is a public repository

`gh repo view` → `visibility=PUBLIC`, and `.github/workflows/desktop.yml`
triggers on `pull_request` with no fork restriction. Any GitHub user can open a
pull request whose head branch contains arbitrary workflow-invoked code
(build scripts, `build.rs`, test code, npm lifecycle scripts).

On GitHub-hosted runners that is contained: the VM is ephemeral, isolated, and
destroyed after the job. On a self-hosted runner that same PR executes attacker
-controlled code **on your hardware**, with access to whatever the runner user
can reach. GitHub's own documentation is unambiguous that self-hosted runners
should not be used with public repositories for exactly this reason.

Mitigations exist but each has a real cost:

| Mitigation | Cost |
|---|---|
| Require approval for all outside-collaborator PRs | Manual gate on every fork PR; a mis-click is full code execution |
| Restrict the workflow to `pull_request_target` / same-repo branches | Fork PRs get **no** CI — loses the verification the gate exists to provide |
| Ephemeral (just-in-time) runners in a throwaway VM per job | Rebuilds the isolation GitHub already provides, on hardware you now operate |
| Make the repository private | Contradicts the fork's source-transparency purpose |

None of these is attractive for a public fork whose CI value is precisely that
it verifies incoming changes.

## Would it even help? Measured evidence says the upside is small

From the Wave 2 measurement (runs `30595777961` and `30595961105`, same change):

- sccache hit rate **99.39%** (653 hits / 4 misses / 0 errors) on **both** runs.
- Queue time 7–8s — negligible; there is no runner-availability problem to fix.
- Execution nonetheless varied 142s → 266s (**1.9×**) on identical cached work.

So compile caching is already saturated; the remaining variability is runner
CPU/IO contention. A dedicated machine would plausibly remove that ~2× swing and
the macOS-minute billing multiplier — a real but bounded win, against operating
a build host and accepting the exposure above.

The honest summary: the gain is *convenience and cost*, the risk is *arbitrary
code execution on owned hardware*. That trade does not favor adoption here.

## If it is ever revisited, the design must cover

Recorded so a future decision starts from requirements rather than a blank page.

**Isolation & security**
- Ephemeral, just-in-time runners: fresh workspace per job, destroyed after; never a long-lived shared checkout.
- Dedicated unprivileged user; no access to the operator's keychain, SSH keys, iCloud, or other repositories' working copies.
- Repository-scoped registration only — never org/user-level runners shared across repositories, which would let one repository's job reach another's cache and artifacts.
- Fork PRs must remain on GitHub-hosted runners (label-routed), or be excluded entirely.

**Caching**
- Repository-scoped cache namespace, keyed as today: `grokptah-v1-${os}-${arch}-rust-<toolchain>-${hashFiles(Cargo.lock…)}`.
- A local sccache disk store is the main upside (no remote read/write), but it must be bounded (`SCCACHE_CACHE_SIZE`) and periodically pruned.
- **Never** share a writable Cargo `target/` directory between concurrent jobs — the same rule `#202` established for local worktrees; concurrent `cargo` on one target directory corrupts or serialises on the lock.

**Capacity & cleanup**
- Concurrency limit of 1 heavy Rust job per machine, or per-job CPU/RAM caps; the measured 2× variance came from contention.
- Disk budget: existing local evidence is ~3.2 GiB desktop target + ~1 GiB bridge target per checkout, and ~22 GiB across active worktrees — a runner needs headroom plus automatic reclamation, or it silently fills.
- Scheduled prune of caches, `target/` dirs, and stale `hdiutil` mounts (see `docs/PACKAGING.md` — leftover DMG mounts break the next packaging run).

**Toolchain drift**
- The runner must pin the same versions CI asserts today (Rust `1.92.0` via `rust-toolchain.toml`, Node 20). A self-hosted host drifts silently as the operator upgrades local tooling; the Wave 1 npm episode is the cautionary case — a lockfile produced by npm 11/Node 25 locally was rejected by CI's older npm. Pin explicitly and verify in-job rather than inheriting the host's state.

**Provenance**
- Release artifacts must be built from the exact reviewed commit on a clean
  checkout (`git status --porcelain` empty). A reused workspace makes that
  claim unverifiable — a further argument for ephemeral workspaces if releases
  are ever built off-hosted.

## Cheaper alternatives, if the motivation is cost

1. Keep the current design (recommended): warm cache is already 99.39% effective; expected PR cost ≈2.5 min.
2. `#203` already removed duplicate push+PR execution per commit — the largest structural saving, already banked.
3. If macOS minutes become the binding constraint, move only the *frontend* job (typecheck + vitest, ~15s) to `ubuntu-latest`; the Rust/Tauri work genuinely requires macOS.
