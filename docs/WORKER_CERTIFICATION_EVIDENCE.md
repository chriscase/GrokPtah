# Independent worker certification evidence

`worker_certification_evidence.rs` defines the secret-free v2 release record
for roadmap stage 6. It is deliberately separate from the worker runtime: a
passing lease-fencing unit test is not a long-running multi-worker campaign.
The earlier v1 shape was never a retained campaign artifact and is rejected:
it did not bind the entire record against tampering or prove that its claimed
soak duration matched elapsed timestamps.

Every record binds an exact assembled SHA and named campaign to:

- at least two distinct worker identities and durable lease coverage;
- crash/restart recovery with at least three process restarts and zero
  duplicate executions;
- per-worker least-privilege credential issuance and rotation, including
  rejection of the old credential and acceptance of the replacement, with one
  distinct credential fingerprint per worker;
- retained audit evidence; and
- a measured wall-clock soak of at least 72 hours whose soak count and
  `operational_soak` check duration cannot exceed the recorded timestamp span.

All checks carry opaque evidence digests, and a whole-record digest binds the
canonical v2 payload. The record never stores bearer tokens, raw credentials,
prompts, source text, endpoints, or private notes. Unknown fields or check IDs,
duplicate identities or credential fingerprints, missing checks, failed
checks, malformed or stale digests, incomplete credential coverage, fewer than
two workers or three restarts, duplicate execution, or a short/overclaimed
soak fail closed. A valid record can still remain non-claiming until the
campaign owner explicitly sets `claim_eligible` after the host evidence is
retained.

This contract does not manufacture live evidence. The release gate still
requires an independently executed, production-shaped campaign.
