# Release artifact evidence handoff

This document records the strongest currently verified desktop-release evidence.
It is an evidence index, not a claim that GrokPtah is fully qualified or ready
for signed distribution.

## Verified candidate

- Repository: `chriscase/GrokPtah`
- Branch: `codex/desktop-release-workflow-parser-fix-v1`
- Candidate head: `d2c44e9aca00a868ab162394623e9a294faa81b7`
- Pull request: [#382](https://github.com/chriscase/GrokPtah/pull/382), still draft
- Hosted workflow: [run 32818915338](https://github.com/chriscase/GrokPtah/actions/runs/32818915338)
- Result: completed successfully

## Gates covered by the hosted run

The run passed all of these gates on the exact candidate head:

1. Clean reviewed checkout.
2. Public package build and external-consumer smoke (`npm run verify:public`).
3. Host-neutral Agent SDK tests (`cargo test --locked -p grokptah-agent-sdk`).
4. Unsigned macOS desktop bundle build.
5. SHA-256 manifest generation and in-run verification.
6. Artifact upload with seven-day retention.

The Agent SDK suite reported 12 passing tests. The consumer smoke exercises the
published Help Center corpus, semantic ranking, bounded assistant streaming,
prompt queue reducer, broker projections, and external-worker contracts.

## Artifact and independent verification

- Artifact: `grokptah-macos-unsigned-d2c44e9aca00a868ab162394623e9a294faa81b7`
- Size: 19,540,320 bytes
- GitHub reports it as non-expired.
- The downloaded artifact's SHA-256 manifest was independently checked against
  every included DMG, app executable, plist, icon, and support file; every entry
  returned `OK`.

## Explicit non-claims

This evidence does **not** prove:

- signed/notarized distribution or updater publication;
- packaged Virtualization.framework launch, guest boot, rendered frames, host
  input, or live-VM cleanup;
- the Always-On Stage 6 soak or the later Stage 3/4/5 campaigns;
- a live Cursor Cloud campaign, manager wiring, or stream/reconnect qualification;
- independent exact-head review, merge, or undraft.

The next qualification campaign must wait until the protected Stage 6 process
exits and its target is clear under both `ps` and `lsof` checks.
