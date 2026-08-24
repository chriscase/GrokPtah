# Packaged Computer Use lease-fence source report — b250b70

**Recorded:** 2026-08-24  
**Decision:** **PASS — source lease-fence checks only**

This is an external, secret-free report for the exact source candidate
`b250b7096c6131721864b39f4cc5fdce5e3ada15`. It is evidence for the denial
boundary, not a packaged Computer Use or VM certification.

## Identity

- Disposable checkout: `/private/tmp/grokptah-cu-packaged-lease-fence-b250b70`
- Branch: `codex/cu-isolated-guest-bootstrap-v1`
- Candidate head: `b250b7096c6131721864b39f4cc5fdce5e3ada15`
- Parent: `295a4ff62939af1a3034119653c83c7a0a2e1bff`
- PR #374 head ancestor: `5919e3343af20a78e17459b8ac8454bbc5aeca7e`
- Developer checkout (untouched):
  `6409645cb7d0fe6d75585f0610366340f808b8ec`
- Worktree was clean; no merge, push, rebase, undraft, PR, or source patch
  occurred.

The report names input bundle SHA-256
`2888bd904e3175e475d105e63e2b9aed449fcf8b25a0081464d8a67df1d74edd`.
That is a different transport bundle from the currently pinned external-
handoff bundle
`4d4f46a85168b45476c1acc47ba7e289bfcb27b6ea08b173d862a038f27a2352`, but
the repository-owned lease-fence verifier independently confirms the pinned
bundle is complete and contains this exact candidate. The alternate digest is
retained for provenance; it is not evidence for the later VM campaign.

## Checks returned

| Check | Result |
| --- | --- |
| `rustfmt --edition 2021 --check` on the changed Rust files | pass |
| `cargo metadata --locked --offline --no-deps --format-version=1` | pass |
| `cargo test --locked --lib isolated_guest -- --test-threads=1` | 8 passed |
| `cargo test --locked --lib native_launch_descriptor_set_must_be_complete_and_unique -- --test-threads=1` | 1 passed |

The isolated-guest compile included the lease-fenced macOS packaged-runtime
supervisor signatures. Those methods were compiled, not executed against a
VM. The optional `docs/verify-packaged-lease-fence.sh` check was absent at
this head and was not treated as a failure.

## Resource evidence

The mandatory `sccache` wrapper, namespaced cache, and shared compatibility
target were used serially. The report records approximately 52 GiB free
before and after, a shared target growing from 2.7 GiB to 2.9 GiB, and zero
`lsof` handles after the run. The target was retained; no protected process or
developer checkout was touched.

After recording the external report, the repository-owned source verifiers
were run read-only on the candidate:

```text
sh docs/verify-packaged-lease-fence.sh
  packaged_lease_fence_source=present
  packaged_lease_fence_candidate=b250b70
  packaged_lease_fence_claim_status=source_only
  packaged_lease_fence_bundle=verified

sh docs/verify-packaged-lease-stop-failure-fence.sh
  packaged_lease_stop_failure_source=present
  packaged_lease_stop_failure_candidate=40730e4
  packaged_lease_stop_failure_claim_status=source_only
  packaged_lease_stop_failure_bundle=verified
```

These are source/documentation verifiers; they do not launch a VM or replace
the v15 packaged qualification campaign.

## What this does and does not prove

The result proves the source-level lease and launch-descriptor checks for the
candidate. It does **not** prove a signed helper or guest image,
Virtualization.framework launch, real guest boot, rendered frames, host input,
live-VM cleanup, multi-agent desktop acceptance, or a long soak. Stage 9 /
issue #288 therefore remains open, and the later v15 full campaign remains the
next packaged/hardware gate.
