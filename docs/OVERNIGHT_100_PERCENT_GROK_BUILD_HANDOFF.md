# Overnight 100% campaign handoff

Status: **coordination procedure only; it does not claim any stage is passed.**

Use this when delegating the remaining GrokPtah qualification work to Grok Build, Cursor Agents,
or another constrained company gateway. It is intentionally evidence-first: a lane that cannot
obtain the authoritative evidence must return `NOT_RUN` or `FAIL`, never an inferred pass.

## Operating rules

1. Freeze one exact candidate SHA before each lane. Use a disposable checkout and never modify the
   developer checkout, existing sessions, source branches, or GitHub unless separately authorized.
2. Before every build, report free disk, active process owners, target path, and cleanup owner. For
   Rust commands use the repository-family cache policy explicitly and serially:

   ```sh
   export RUSTC_WRAPPER=/opt/homebrew/bin/sccache
   export SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
   export CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default
   ```

   Do not create in-checkout or per-agent Cargo targets. Report target size and cleanup status
   after each lane.
3. Never use a hermetic fixture, source verifier, draft PR, or “not observed” provider response as
   live/hardware/soak evidence.
4. Every lane returns a secret-free report containing: exact SHA, lane ID, start/end time, commands,
   artifact paths and SHA-256 digests, PASS/FAIL/NOT_RUN, failure reason, and reviewer role.
5. A failure or missing prerequisite stops only that lane. Do not retry uncertain provider sends,
   guest input, or destructive cleanup automatically. Do not relabel an unmet mandatory exit as
   unsupported.

## Lane order

Run these in dependency order; disjoint read-only audits may run in parallel, but shared build
targets and credentialed runtime hosts are single-owner resources.

### A. Stage 1 — product-head admission

- Verify the frozen candidate contains the independently reviewed Native Coding P1 repairs.
- Confirm the public Run projection has no route-secret leakage, ManagerProposal is deny-all before
  advertisement/dispatch, AgentSpec revisions are frozen, `UncertainAccept` never auto-retries,
  and Run/quota/Agent admission is atomic.
- Confirm PR #352 (or its superseding implementation) is merged before calling the stage passed.
- Run the exact external formatter/lint/test campaign with the required cache policy and retain the
  sealed report. A draft PR remains `NOT_RUN`.

### B. Stage 2 — live Grok Build and provider-quota receipt

- Establish the official `~/.grok/auth.json` session through the approved Grok CLI; do not inject
  API keys, token commands, API-base overrides, or compatible-gateway overrides.
- Require `attest_grok_build_oidc_with_min_validity` to return `certification_ready=true`.
- Run the named live catalog scenarios in `ROADMAP_TO_100.md`, not only hermetic replay.
- Attach one positive, secret-free receipt bound to the campaign, opaque credential fingerprint,
  canonical route/model digest, provider-side consumption, and provider-side exhaustion/HTTP-429.
  “Quota not observed” is a failure, not a pass.

### C. Stages 3–5 — authority, parity, and memory

- Run the sealed Stage 3 least-privilege authority campaign and independently inspect its report.
- Run the authenticated public-HTTP/desktop-loopback/standalone-service parity fixture with its
  current golden; sharing Rust types is not parity evidence.
- Run the exact logical-years memory campaign with its committed fixtures and retention metrics.
- Do not start the Stage 6 soak until Stages 3–5 have exact passing reports.

### D. Stage 6 — durable always-on workers

- Run the real `grokptah-service` process with one service-owned durable home, at least three
  restarts, two independent Agent-bound workers, lease/credential rotation, no duplicate attempts,
  no implicit resume, bounded resources, and the exact 259200-second campaign.
- Retain the unique platform-temp evidence path and validate it with the repository’s evidence
  validator. The short CI smoke or a desktop Manager session is not a 72-hour pass.

### E. Stages 7–9 — Computer Use

- Qualify the packaged semantic macOS fixture and packaged identity/hardware matrix.
- Run the isolated visual VM handoff in the current
  `COMPUTER_USE_ISOLATED_GROK_BUILD_HANDOFF_V15.md` (the older v12–v14 handoffs
  are explicitly superseded); a signed VM must boot, render, accept bounded
  guest-only pointer/key/text input, stop/crash/restart safely, and prove exact cleanup while the
  host pointer, foreground app, window, clipboard digest, and unrelated windows remain unchanged.
- Keep raw global injection, clipboard, shell, host input, and unattended grants unsupported.

### F. Stages 10–12 — UX, operations, enterprise review

- Run the packaged expert UI review matrix and recurring cadence record for wide/narrow,
  light/dark, loading/error/quota/authority/reconnect, keyboard, screen reader, focus, contrast,
  zoom/reflow, and reduced motion. Mockups alone do not qualify.
- Execute the operations drill: backup/restore, upgrade/rollback, restart, cursor expiry,
  disk-full/corrupt/torn state, sole-writer contention, monitoring, RTO/RPO, and packaged Stop/
  Take over.
- Run the enterprise-gateway review lane with a signed lease, separate trust record, egress
  attestation, premium fallback disabled, read-only workspace/publication policy, seven bounded
  specialist passes, durable restart/retry identity, and a paired multi-hour quality result.
  Never silently route to a stronger model.

## Final decision format

Return a table with one row per stage and exactly one of `PASS`, `FAIL`, or `NOT_RUN`. A final
`100%` recommendation is allowed only when every mandatory row has authoritative evidence attached,
all open release-gate issues are closed or explicitly accepted with owner/date, and the final
packaged build, docs, capability matrix, roadmap, and UI/operations records bind the same exact
candidate SHA. Otherwise return `NOT 100%` and list the next blocking artifact.
