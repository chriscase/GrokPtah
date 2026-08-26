# Post-soak qualification and release identity

**Status:** Verifier only. No Stage 6 evidence report exists in this repository,
and this gate has never been run against a real soak artifact.

The Stage 6 Always-On soak is the source of truth for exit gate 1 in
[ROADMAP_TO_100.md](./ROADMAP_TO_100.md). Until now that gate was prose: a
person read a soak's output and decided whether it counted.
[`grokptah-release-evidence`](../crates/common/grokptah-release-evidence)
makes the decision mechanical instead. It does not produce evidence, and it
cannot stand in for a soak that has not run.

## What the gate reads

Exactly one file, in a directory that contains nothing else:

```text
<evidence-dir>/
  post-soak-qualification.json     # any file name; there must be exactly one
```

The directory must hold one entry, that entry must be a regular file, and it is
classified without following symlinks. A missing report, a second file, a
subdirectory, a symlink, and a file over
[`MAX_REPORT_BYTES`](../crates/common/grokptah-release-evidence/src/verify.rs)
are each a rejection.

## Running it

```sh
cargo run -p grokptah-release-evidence --bin grokptah-qualify-release -- \
  <evidence-dir> <policy.json> [artifacts.json]
```

Exit `0` prints the qualification, and the bound release record when artifact
metadata is supplied. Exit `1` prints every finding. There is no partial
success and no warning state.

`policy.json` pins the exact identity and the thresholds the candidate must
clear:

```json
{
  "expectedCandidateHead": "<40-character lowercase hex commit>",
  "expectedParentHead": "<40-character lowercase hex commit>",
  "minimumSoakSeconds": 86400,
  "minimumWorkers": 3,
  "minimumRestarts": 2,
  "minimumAuditRecords": 1000,
  "maximumReportAgeSeconds": 3600,
  "allowedScopes": ["audit:append", "run:execute", "run:read"]
}
```

The policy is checked before any evidence is weighed against it: both heads must
be full lowercase hex and must differ, so a policy that cannot pin an exact
candidate is rejected rather than producing a confusing identity mismatch.

Two floors are properties of the checks themselves and a policy cannot lower
them: at least `MINIMUM_CERTIFIED_WORKERS` (2) certified workers, because one
worker cannot demonstrate distinct workers and distinct bindings, and at least
`MINIMUM_RESTARTS` (1) restart, because a soak that never restarted has not
exercised restart recovery. Setting either policy floor to zero does not relax
them.

## The seven ordered checks

A report declares exactly these seven checks, exactly once each, in this order.
Missing, extra, and reordered checks are rejected.

| # | Check | What the measurements must show |
| --- | --- | --- |
| 1 | `soak_exit_marker` | The terminal exit marker was written; zero processes and zero open handles were still owned at exit; the duration is `measured`, the configured duration clears the policy floor, and the measured duration is not short of it |
| 2 | `worker_isolation` | At least the required number of workers, every worker id distinct, every credential binding distinct, and every worker actually executed |
| 3 | `credential_lifecycle` | One credential issued per certified worker, every scope inside the least-privilege allowlist, no privileged scope requested, at least one rotation, every rotated-out credential rejected, and every rotated-in credential accepted |
| 4 | `restart_recovery` | At least the required restarts, no uncertain resume, no leaked worker |
| 5 | `duplicate_suppression` | Zero duplicate executions, per worker |
| 6 | `audit_retention` | At least the required audit records still readable, none dropped, and retention held across every restart |
| 7 | `evidence_integrity` | No secret marker anywhere in the raw bytes, the report does not assert its own qualification, and the declared digest equals the digest recomputed from the report body |

## Why a declaration is never enough

Each check is evaluated twice, from two independent directions:

1. The verifier computes the outcome from the measurements the report carries.
2. The verifier then requires the writer's declared `passed` flag to agree.

A check declared passing that the measurements do not support is rejected, and
so is a check the writer recorded as failing. This is what keeps a report-only
inference from becoming a qualification.

Three further rules close the same gap:

- **Nothing is defaulted.** The report types carry no serde defaults and reject
  unknown fields, so an omitted measurement fails to parse rather than reading
  as zero, and evidence the schema does not define is refused rather than
  ignored.
- **A report may not qualify itself.** `claimState` must be
  `pending_verification`. A report that arrives already saying `qualified` is
  rejected outright; only the verifier can produce that verdict.
- **Qualification cannot be deserialized into existence.** `QualifiedCandidate`
  and `ReleaseRecord` have no `Deserialize` implementation and no public
  constructor, so holding one means having passed verification.

## Identity, freshness, and integrity

- `candidateHead` and `parentHead` must be full 40-character lowercase hex, must
  match the policy exactly, and must differ from each other. An abbreviated or
  differing commit is a rejection, so evidence from an older head cannot be
  reused for a newer one.
- A report dated after verification time is rejected, and one older than
  `maximumReportAgeSeconds` is stale.
- `evidenceDigestSha256` is SHA-256 over a canonical encoding of the report
  body — every field except the digest, in a fixed order. The verifier
  recomputes it and never accepts the declared value on its own. The encoding is
  a fixed-order struct rather than a dynamic map, so it does not depend on
  `serde_json` map-ordering features enabled elsewhere in the dependency graph,
  and an external writer can reproduce it.
- The raw report bytes are scanned for secret markers before parsing, so a
  secret in a malformed or unparsed region is still caught.

## Release identity

A release record can only be produced by binding artifact metadata to a
`QualifiedCandidate`, so it always names the exact commit whose evidence passed.
Binding validates every artifact — plain file name, non-zero length, lowercase
hex SHA-256, no repeated name — canonically orders them, and seals the result
with a release digest over the whole body. The record exposes read-only
accessors and cannot be edited after binding.

## What this does not prove

Passing this gate proves one thing: a completed soak wrote evidence that clears
the policy for an exact head. It says nothing about the live provider, gateway,
packaged Computer Use, hardware, UI, or cross-repository gates in
[ROADMAP_TO_100.md](./ROADMAP_TO_100.md), and it is not a substitute for the
[independent review protocol](./INDEPENDENT_REVIEW_PROTOCOL.md).
