# Enterprise gateway review lane — external certification handoff

Status: **certification procedure only; this document makes no live quality or
enterprise-lane claim.** A report that omits a required gate is a failed or
indeterminate run, not a partial certification.

This handoff is for an operator who has a company-approved, OpenAI-compatible
gateway and a deliberately modest model. The point of the exercise is to prove
that GrokPtah's bounded orchestration can produce a useful long-running review
without routing any request to a stronger provider, leaking code outside the
company boundary, or gaining write/publication authority.

## Exact source and non-negotiable boundaries

Run from a disposable checkout at this exact candidate commit:

- source head: `c7423a0a8476551162c51b4311256978702baaa5`
- candidate branch: `codex/cu-isolated-guest-bootstrap-v1`
- expected repository: GrokPtah

Before doing anything else, record the absolute checkout path, `git rev-parse
HEAD`, `git status --short`, and the source tree's SHA-256 identity. Refuse the
run if the checkout is dirty, the head differs, or the campaign files are not
the checked-in versions. Do not modify the developer checkout, main branch,
GitHub, or a user's source tree.

The only provider route allowed is the broker-issued lease supplied through:

```sh
GROKPTAH_ENTERPRISE_REVIEW_LEASE=/absolute/disposable/review-lease.json
GROKPTAH_ENTERPRISE_REVIEW_TRUST=/absolute/disposable/review-trust.json
```

The lease and trust files must be regular, non-symlink files with bounded size.
They contain no bearer, API key, endpoint URL, or provider response. The
operator-owned broker must sign the lease, and the trust record must contain
the independently selected public key. Never put a secret in a prompt, source
file, artifact, log, report, or Git commit.

The run must fail closed when any of these changes: route or endpoint
fingerprint, credential principal fingerprint, model, effort, capability,
egress policy, validity window, request/token/time budget, read-only policy, or
fallback setting. A company gateway's quota is its own quota; it is not Grok
Build quota and must not be described as such.

## Build and resource preflight

Before any Rust build, report disk headroom and active Cargo/rustc/sccache
owners. Use the shared compatible target serially; do not create a target under
the checkout and do not start a second owner for the same target family:

```sh
export RUSTC_WRAPPER=/opt/homebrew/bin/sccache
export SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
export CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default

test -x "$RUSTC_WRAPPER"
checkout=$(pwd -P)
case "$(cd "$CARGO_TARGET_DIR" 2>/dev/null && pwd -P)" in
  "$checkout"|"$checkout"/*) echo target_must_not_be_inside_checkout >&2; exit 1;;
esac
df -h "$checkout" "$CARGO_TARGET_DIR" 2>/dev/null || df -h "$checkout"
ps -axo pid,ppid,command | rg 'cargo|rustc|sccache' || true
```

After the run, report target path and size. Remove only an isolated target that
has no matching process or open handle; preserve the shared family target for
serial reuse. Do not kill a pre-existing sccache daemon merely to make the
campaign look clean.

## Required campaign

Use the checked-in held-out campaign, not a hand-written substitute:

`evals/code-review-benchmark/campaign.v1.json`

It binds 24 cases across eight project families, two live replicates, a fresh
single-pass baseline, and a persistent-per-family GrokPtah arm. It denies
shell, write, MCP, Computer Use, and publish tools. The review workspace must
be disposable, read-only, and unchanged before and after the run.

First perform the admission-only check:

```sh
cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  review --repository "$PWD" --live --preflight
```

Admission is not certification. Only proceed to a live campaign if the output
contains the real broker lease, gateway deployment attestation, and external
egress-firewall attestation. Ambient API keys, provider discovery, compatible
gateway auto-detection, and fallback routes are refusals.

The live campaign must produce all of the following, with timestamps and exact
digests:

1. Two paired baseline/GrokPtah replicates over the complete 24-case corpus.
2. Seven bounded specialist passes where relevant: correctness, security,
   concurrency, performance, tests, API, and UX.
3. Durable checkpoint and restart evidence showing no implicit resend,
   duplicate finding, or loss of route/policy binding.
4. Route-drift denial while a review is in flight, requalification required
   before resumption, quota-one-under admission, quota-exhausted denial, and
   quota-window reset behavior.
5. Read-only proof: unchanged workspace Merkle root and Git refs, no remote
   publication, no forbidden tool call, and no canary or secret in requests or
   evidence.
6. Authoritative provider usage/quota receipts that identify the same route,
   credential principal, model, and deployment as the signed lease, without
   exposing secrets.
7. A public report with exact file/symbol/region locations, confirmed versus
   hypothesized findings, confidence, model limitations, deduplication, and
   evidence-grounded synthesis.

The sealed campaign thresholds are the ones in `campaign.v1.json`: minimum
precision 0.75, weighted recall 0.75, high-critical recall 0.85, usefulness
0.70, paired weighted-utility lift 0.15 with lower confidence bound 0.08,
recall lift 0.15 with lower bound 0.05, at least six of eight family wins, and
no family materially worse than the allowed bound. Token, request, wall-time,
continuation, and artifact limits must also remain within the manifest.

## Evidence and independent review

Write the report only to a disposable, operator-selected output directory.
Run the repository's report verifier and preserve the command transcript, but
publish only the secret-free projection. The handoff must include:

- exact source head and campaign/corpus/scorer/runner digests;
- lease route/model/credential/deployment fingerprints and attestation flags;
- authoritative usage receipt digest and quota outcome;
- paired metrics, confidence intervals, family win table, and baseline delta;
- restart, route-drift, quota, mutation-denial, egress, and cleanup evidence;
- independent reviewer identity and disposition.

An independent reviewer must inspect the exact sealed report and confirm that
the observed provider was the modest company gateway, that no fallback or
publication occurred, and that the quality result meets the manifest. The
reviewer must be able to mark the result **fail**, **indeterminate**, or
**certified**; an implementation author cannot self-certify.

## Fail-closed outcomes

Stop and report `NOT_QUALIFIED` if the lease is missing or stale, the route is
not frozen, usage is unauthenticated, the gateway cannot run the paired
campaign, any mutation/egress occurs, a restart duplicates work, a denial test
is absent, a threshold fails, the evidence is incomplete, or the independent
review is missing. Do not create a golden, update the capability matrix to
Supported, or claim Stage 12 from a preflight, fake-provider run, one chat turn,
single-pass frontier model, or Grok Build result.

The corresponding roadmap row remains **Unverified** until every live and
independent-review gate above is satisfied.
