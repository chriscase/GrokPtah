# Stage 6 evidence-hardening independent security review

Recorded 2026-08-24 from a separate read-only Grok review of exact head
`984ff9a4b13a6f2eb2054c84d5880abd5a0d4e1a` (parent
`5406bbea059371392b0d77d58cca083640244a6c`, reviewed range `a13a048..HEAD`).
The reviewer made no edits, builds, tests, commits, pushes, or repository
mutations.

## Verdict

**PASS-WITH-BLOCKERS.** Credential parsing, workspace binding, rotation, and
the v2 evidence contract are internally coherent and mostly fail closed. The
review does not qualify Stage 6, #305, or the 72-hour campaign.

## Findings that must be closed

1. **Cross-worker denial is not identity proof.** The runner accepts an
   already-leased conflict as success and never exercises worker B presenting
   worker A's identity or lease token. The runtime re-check exists, but the
   Stage 6 runner does not prove it (`tests/always_on_grokbot.rs:1035-1056`,
   `crates/codegen/grokptah-service/src/service.rs:3566-3573,3814-3821`).
2. **Least privilege is self-attested while worker credentials remain
   coordinator-shaped.** The validator checks booleans, while create/cancel
   and an unbound assignment path do not consult the bound worker identity
   (`tests/always_on_grokbot.rs:780-824,1188-1191`,
   `crates/codegen/grokptah-service/src/service.rs:3286-3314,3845-3876,3913-3915`).
3. **Restart count is not lease-bearing recovery count.** The three required
   restarts include credential installation and rotation outside an in-flight
   lease, so the record can validate without three crash recoveries while work
   is leased (`worker_certification_evidence.rs:24-25,217-220`,
   `tests/always_on_grokbot.rs:2754-2800`).
4. **Secret scanning does not cover all MCP bodies.** The current sentinel scan
   covers selected files, stderr head/tail, and final reports, but a high-
   entropy worker bearer echoed in an MCP result would not necessarily fail
   the campaign; stderr middle is also unscanned
   (`tests/always_on_support/mod.rs:1703-1857`).
5. **Documentation is contradictory.** `docs/COORDINATOR_WORKERS.md:33-34`
   still describes production-shaped issuance and long-running evidence as
   open while later sections describe the candidate runner. This must be
   reconciled before a release claim.

## External evidence still required

- Run the exact-head bounded Stage 6 smoke with the mandated external target
  and `sccache` environment.
- Independently verify Stage 3, 4, and 5 artifacts before any 72-hour run.
- Run the exact 259200-second campaign on an unchanged clean head and retain
  a sole-writer, secret-free artifact whose digest is checked by an inspector
  who did not run the campaign.
- Add direct proof that worker B cannot claim with A's identity, complete or
  progress A's lease, or mutate after lease expiry; do not treat conflict
  alone as identity isolation.
- Scan complete MCP bodies, complete stderr, persisted files (including
  non-UTF-8 handling), and the retained report for credentials, paths, and
  endpoint/base-URL material.

Until these blockers and external gates are closed, Stage 6 remains
**Experimental / Unverified** and no 100% claim is permitted.
