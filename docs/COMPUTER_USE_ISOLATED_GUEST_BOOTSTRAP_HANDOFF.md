# Isolated guest bootstrap — source handoff

Status: **source-level proof only. This is not packaged VM hardware certification.**

Worked from isolated branch `codex/cu-isolated-guest-bootstrap-v1` at expected
head `295a4ff62939af1a3034119653c83c7a0a2e1bff`. The later local checkout at
`6c6d1e7` was not used or modified. `main` was not modified.

## What this adds

The existing Isolated Visual lifecycle (`Prepared` → `Starting` →
`ReadOnlyReady` → `Stopping` → `CleanupPending` → `Terminated`) is unchanged.
This slice adds the smallest guest-bootstrap proofs that were missing on that
contract:

1. Explicit guest phases `create` / `ready` / `running` / `closing` / `failed`
   as a closed projection over the helper+lifecycle pair. `running` is helper
   `Bound`; it is not a rewritten lifecycle enum.
2. One Agent per guest lease. A second Agent cannot acquire or control a live
   guest. Control without a lease is denied. A stale lease revision is rejected
   without mutating the guest.
3. Cancel and helper failure revoke the lease and still require exact cleanup
   evidence. There is no resume after close or failure.
4. Captured-artifact projection drops frame bytes. Metadata redaction strips
   path, clipboard, credential, and network keys and fail-closes on leftover
   needles.
5. The macOS packaged-runtime supervisor now carries the same exact
   one-Agent lease fence: start, frame reads, guest input, and stop require a
   matching lease; failure revokes it; cleanup is refused until the lease is
   gone. This is still an unexposed source seam until packaged qualification.

Durable `ComputerSurfaceLease` records and Isolated Visual denial boundaries
are not replaced or relaxed.

## Proven by focused tests

- `computer_use::isolated_guest` simulator tests: concurrent agents, stale
  leases, control-without-lease, cancel cleanup, guest failure cleanup, capture
  redaction.
- `failure_after_committed_stop_still_requires_exact_cleanup` on the existing
  lifecycle (post-stop failure cannot reopen a run).

## Explicitly unverified

- Packaged VM hardware / Virtualization.framework launch
- Real guest image boot, rendered frames, or host-input into a VM
- Long soak
- Multi-agent desktop acceptance on a live macOS surface
- Signed helper packaging and entitlement measurement as a qualification result

The packaged-runtime lease follow-up is candidate commit `HEAD` after the
PR integration; it is not part of PR #374’s GitHub allowlist and does not
change the source-only qualification status above.

The reproducible external compile/test procedure for that follow-up is
[`COMPUTER_USE_PACKAGED_LEASE_EXTERNAL_HANDOFF.md`](COMPUTER_USE_PACKAGED_LEASE_EXTERNAL_HANDOFF.md).
