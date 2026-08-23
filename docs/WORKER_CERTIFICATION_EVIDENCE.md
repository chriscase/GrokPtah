# Independent worker certification evidence

`worker_certification_evidence.rs` defines the secret-free release record for
roadmap stage 6. It is deliberately separate from the worker runtime: a
passing lease-fencing unit test is not a long-running multi-worker campaign.

Every record binds an exact assembled SHA and named campaign to:

- at least two distinct worker identities and durable lease coverage;
- crash/restart recovery with a positive restart count and zero duplicate
  executions;
- per-worker least-privilege credential issuance and rotation, including
  rejection of the old credential and acceptance of the replacement;
- retained audit evidence; and
- a measured wall-clock soak of at least 72 hours.

All checks carry opaque evidence digests. The record never stores bearer
tokens, raw credentials, prompts, source text, endpoints, or private notes.
Unknown fields, duplicate identities, missing checks, failed checks, malformed
digests, incomplete credential coverage, duplicate execution, or a short soak
fail closed. A valid record can still remain non-claiming until the campaign
owner explicitly sets `claim_eligible` after the host evidence is retained.

This contract does not manufacture live evidence. The release gate still
requires an independently executed, production-shaped campaign.
