# Enterprise gateway review lane — current-candidate handoff

Status: **queued certification procedure only; no live enterprise quality
claim is made.** This v2 handoff pins the current candidate and supersedes the
older `ENTERPRISE_REVIEW_V1_HANDOFF.md` source cutoff.

## Frozen input

- Immutable source bundle: `/private/tmp/grokptah-enterprise-review-v2.bundle`
- Bundle SHA-256: `c3d5598fae032d540ca255220a06dfe3c0566cf1ce557d20dcf5e96136da203f`
- Source ref: `codex/cu-isolated-guest-bootstrap-v1`
- Source cutoff: `a32657cd95a65b3b7b0c287929aff599a3c46a95`
- Developer checkout (must remain untouched):
  `6409645cb7d0fe6d75585f0610366340f808b8ec`

The bundle has complete history and passes `git bundle verify`. The handoff
file itself is documentation added after sealing; the bundle is the only source
input for the campaign.

## Copyable external prompt

```text
Run the GrokPtah Stage 12 enterprise gateway review certification from the
exact immutable bundle below. This is a fail-closed live campaign, not an
implementation task.

Bundle: /private/tmp/grokptah-enterprise-review-v2.bundle
Bundle SHA-256: c3d5598fae032d540ca255220a06dfe3c0566cf1ce557d20dcf5e96136da203f
Source cutoff: a32657cd95a65b3b7b0c287929aff599a3c46a95

Create a disposable checkout, verify bundle SHA/complete history/exact HEAD/
clean worktree, and preserve the developer checkout, Git branches, GitHub,
existing app sessions, and all other campaigns. The reviewed user is restricted
to one company-approved OpenAI-compatible gateway and a deliberately modest,
non-frontier model. Never silently route to Grok Build or any stronger model.

The only route authority is the operator-broker signed lease and separate trust
record supplied through regular, non-symlink, bounded files:
GROKPTAH_ENTERPRISE_REVIEW_LEASE=/absolute/disposable/review-lease.json
GROKPTAH_ENTERPRISE_REVIEW_TRUST=/absolute/disposable/review-trust.json
They contain no bearer, endpoint URL, API key, provider response, or private
signing material. Reject missing/stale/malformed/unsigned/over-broad files,
route/model/credential/deployment drift, fallback enabled, missing egress
attestation, write/network/publication permission, and budget overruns before
the first provider turn.

Before every Rust command set exactly:
RUSTC_WRAPPER=/opt/homebrew/bin/sccache
SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default
Reuse that target serially. Report disk, active cargo/rustc/sccache owners,
target path/owner before building and target size, lsof/open handles, and
cleanup/retention afterward. Do not create an in-checkout or per-agent target.

Use the checked-in held-out campaign `evals/code-review-benchmark/campaign.v1.json`
and the certification-lab live preflight, not a hand-written substitute:

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  review --repository "$PWD" --live --preflight

Proceed only if the output contains the real broker lease, gateway deployment
attestation, and external egress-firewall attestation. Then run the complete
24-case corpus with two paired live replicates, fresh single-pass baseline,
persistent-per-family GrokPtah arm, and seven bounded specialist passes:
correctness, security, concurrency, performance, tests, API, and UX.

Retain secret-free evidence for durable checkpoint/restart with no duplicate or
implicit resend; route drift denial and requalification; quota-one-under,
quota-exhausted and reset behavior; read-only workspace Merkle/refs unchanged;
zero forbidden tool calls, network egress, publication, or secrets; and exact
cleanup. Include authoritative provider usage/quota receipts bound to the
signed route, credential principal, model and deployment.

The sealed thresholds in campaign.v1.json are mandatory: precision >= 0.75,
weighted recall >= 0.75, high-critical recall >= 0.85, usefulness >= 0.70,
paired utility lift >= 0.15 with lower bound >= 0.08, recall lift >= 0.15 with
lower bound >= 0.05, at least six of eight family wins, and no materially worse
family. Respect all request/token/time/artifact limits.

Obtain an independent reviewer who did not implement the lane. The reviewer
must confirm the modest company gateway was actually used, no fallback or
publication occurred, all denial/restart/quota evidence is present, and the
quality thresholds are met. A preflight, fake provider, one chat turn, frontier
single-pass result, or Grok Build result is not certification.

If any gate is absent, mismatched, unauthenticated, mutated, egressed, below
threshold, or independently unreviewed, return NOT_QUALIFIED and stop. Do not
update the capability matrix or claim Stage 12. Return one dated, secret-free
report with exact source/campaign/corpus/scorer/runner digests, lease/trust
fingerprints and attestation flags, usage receipt digest, paired metrics,
family results, restart/route-drift/quota/denial/cleanup evidence, reviewer
role, and explicit CERTIFIED or NOT_QUALIFIED decision. Attach it only to
source cutoff a32657cd… .
```

## Interpretation

Only the complete live paired campaign plus independent review can advance
Stage 12. Company-gateway quota is that provider's quota; it is not Grok Build
quota or account-balance synchronization.
