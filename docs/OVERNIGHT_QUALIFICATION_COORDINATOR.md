# Overnight qualification coordinator

Status: **copyable orchestration procedure; no certification is claimed by
this document.** This coordinator serializes the two currently prepared
external gates so they cannot contend for the shared Rust target or be
mistaken for one another.

## Copyable Grok Build prompt

```text
Run these two independent GrokPtah qualification lanes serially overnight.
This is a fail-closed evidence campaign, not an implementation task.

Global rules for both lanes:
- Preserve /Users/chriscase/Documents/GitHub/GrokPtah, all Git branches,
  GitHub, existing app sessions, and every other campaign.
- Never merge, push, rebase, undraft, create a PR, patch a disposable checkout,
  or silently substitute another source/ref.
- Before every Rust command, report disk headroom and active cargo/rustc
  owners, then set exactly:
  RUSTC_WRAPPER=/opt/homebrew/bin/sccache
  SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
  CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default
- Reuse that target only serially. Never create an in-checkout or per-agent
  target, kill another owner, or kill the shared sccache daemon. Report target
  size, owner, lsof/open handles, and cleanup/retention after each lane.
- If any identity, process-ownership, security, or cleanup precondition is
  missing, return NOT_QUALIFIED for that lane and stop that lane.

Lane A — Always-On v52 (Stage 6 / #301, #305):
- Use only this immutable bundle:
  /private/tmp/grokptah-dream-stage4-v52-public-run-correction.bundle
- Bundle SHA-256:
  56dad64886b77195ad5dac3fe48d4c9cec12dd7c96014bc7d23ee21888f44a0b
- Select exact candidate:
  6e9ee187a846f26c3210ac2e417ed16115813cad
- Follow docs/ALWAYS_ON_GROKBOT_V52_ROUTE_IDENTITY_HANDOFF.md exactly.
- Run the focused regression and real grokptah-service short campaign first.
  Every ptah_get_run must return a redacted PublicRun; MCP internal is an
  immediate NOT_QUALIFIED result.
- Only after the focused phase passes, run the documented full suite, lab
  probe, and retained soak. Preserve secret-free restart/no-duplicate,
  route-redaction, quota, cleanup, and usage evidence.
- Return one dated report with exact source/binary SHAs and explicit
  QUALIFIED or NOT_QUALIFIED. Do not claim Stage 3–6 from a source pass.
- After the report, prove no cargo/rustc/lsof owner remains on the shared
  target. Clean only disposable generated output, not source or shared caches.

Lane B — isolated visual v15 (Stage 9 / #288):
- Start only after Lane A has stopped and the shared target is demonstrably
  free. Lane A failure does not become Lane B evidence; preserve both reports.
- Use only this immutable bundle:
  /private/tmp/grokptah-cu-isolated-visual-v15.bundle
- Bundle SHA-256:
  34ecdcdacf6c07b07d425e56c0f908ba8f6a5932d75f0dd2abb88c5c30bb8012
- Source cutoff:
  2142287f67fe532a631d72f280c91bb8eae38b22
- Follow docs/COMPUTER_USE_ISOLATED_GROK_BUILD_HANDOFF_V15.md and the
  repository-owned runbook exactly.
- Require the exact signed helper/image, Virtualization.framework launch,
  real guest boot, rendered-frame and bounded-input evidence, foreground/
  pointer/clipboard invariants, Stop/Take-over/restart/failure negatives,
  exact cleanup, and independent security/accessibility/expert UI review.
- If the host lacks the required virtualization or signing capability, return
  NOT_QUALIFIED with the first exact blocker and stop; do not infer capability
  from source tests or launchAttempted=false.
- Return one dated secret-free report with package/guest/configuration/
  manifest digests, lifecycle evidence, reviewer roles, and explicit
  QUALIFIED or NOT_QUALIFIED. Do not close #288 from partial evidence.

Final response: provide both lane reports, exact source/bundle identities,
resource ownership/cleanup results, and a short list of gates that remain
open. Do not edit the capability matrix or claim 100%.
```

## Interpretation

The coordinator is intentionally serial. A v52 service-process result cannot
qualify isolated visual Computer Use, and a v15 packaged result cannot qualify
Always-On or enterprise-gateway behavior. The final project claim remains
blocked until every roadmap stage has its own retained evidence.

