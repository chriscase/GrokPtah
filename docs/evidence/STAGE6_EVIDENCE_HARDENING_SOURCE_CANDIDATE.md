# Stage 6 evidence-hardening source candidate

Recorded 2026-08-24 from a separate Grok Build checkout. This is provenance
for a source candidate only; it is not an integrated change, a certification,
or a 100% claim.

## Identity

- Checkout: `/private/tmp/grokptah-stage6-evidence-hardening`
- Branch: `codex/stage6-evidence-hardening-v1`
- Worktree HEAD: `984ff9a4b13a6f2eb2054c84d5880abd5a0d4e1a`
- Worktree parent: `5406bbea059371392b0d77d58cca083640244a6c`
- Worktree: clean at inspection
- Original external bundle: `/private/tmp/grokptah-stage6-evidence-hardening-v1.bundle`
- Original external bundle SHA-256: `4ac406378675df055370d3fdf1749cbf4e61379bb1af8faa73980a6e725d58b5`
- Corrected exact-head bundle: `/private/tmp/grokptah-stage6-evidence-hardening-v1-exact-984ff9a.bundle`
- Corrected bundle SHA-256: `cfa741b67c51bc9804b566440a855348727d398c7d97279b2e61e5cbeb12b91b`

The original external bundle advertised head
`05dc61d88bd2c592c7749eeb334d672c2b8b2ddd`, while the inspected worktree was
at `984ff9a4b13a6f2eb2054c84d5880abd5a0d4e1a`. That stale artifact remains
rejected. A corrected bundle was resealed from the clean worktree at the exact
head above; `git bundle verify` reports a complete history.

## What the source candidate contains

The candidate is a 15-file Stage 6 evidence-hardening slice (1582 insertions,
93 deletions from `a13a048`) with a real-process multi-worker soak runner,
scoped worker provisioning across service restarts, and a worker evidence
contract. Its source documentation describes bounded standalone-service smoke
and one accepted-request restart fencing with a loopback fake provider.

Allowed static verification on the exact clean source head passed on
2026-08-24: `rustfmt --edition 2021 --check` over the eight changed Rust files
and `cargo metadata --locked --offline --no-deps`. No local compilation or
test execution was performed; those remain external Grok Build gates.

## Explicit non-claims

The source documentation explicitly records that this candidate has no live
xAI/provider run, no retained 10-minute, 24-hour, or 72-hour artifact, and no
packaged Always-On certification. It therefore does not close Stage 6, #305,
the 72-hour operational soak, provider quota evidence, or any later roadmap
stage. It is not integrated into the current dream candidate and no merge,
push, rebase, PR update, or source patch was performed by this audit.

## Required next gate

1. Run the exact external Stage 6 procedure against the corrected bundle
   above, with the mandatory namespaced
   `sccache` and external `CARGO_TARGET_DIR` settings, serially owning the
   shared target.
2. Obtain an independent review and retain the secret-free campaign artifact
   outside the repository. Until all of that exists, keep Stage 6 explicitly
   open and do not update capability status to PASS.
