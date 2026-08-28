# Isolated visual / VM lifecycle (#288)

Status: source and simulator candidate. **Not VM qualification.**

GitHub issues are authoritative: [#288](https://github.com/chriscase/GrokPtah/issues/288)
owns isolated guest/VM lifecycle and evidence. [#363](https://github.com/chriscase/GrokPtah/issues/363)
owns host-issued surface leases and conflict domains. [#274](https://github.com/chriscase/GrokPtah/issues/274)
remains the cross-cutting adversarial gate. [#444](https://github.com/chriscase/GrokPtah/issues/444)
owns packaged helper/TCC identity. This lane does not edit [#443](https://github.com/chriscase/GrokPtah/issues/443)
headless/broker work or [#435](https://github.com/chriscase/GrokPtah/issues/435) adaptive profiles.

Base: `origin/main` `67e29bd34dc64049432c715c93c2cef2185c63ea`.

PR #374 `5919e3343af20a78e17459b8ac8454bbc5aeca7e` and PR #399
`097301de1d612696afd079ad6e28705427f85fab` are untrusted donor material. Neither
was merged wholesale. Isolated visual was reconstructed onto `main` as
`grokptah-isolated-visual` plus a thin bridge adapter.

## Decision record

Hidden windows, macOS Spaces, virtual-display-only sessions, and any OS
facility that can move the user's real pointer or foreground app **do not**
satisfy isolation. The selected substrate is a disposable guest with a
separate principal (Virtualization.framework when eligible). Until signed
helper/image artifacts, supported hardware, permission state, and 25 GiB free
disk exist, launch fails closed. The deterministic simulator remains the
executable fixture.

## Threat model (isolated guest)

```
untrusted guest pixels, input proposals, source objects, donor Git history
                              |
                              v
     hermetic content-addressed resolver (no ambient Git/index/hooks/alternates)
                              |
                              v
 trusted host: guest identity, helper identity, surface incarnation,
 conflict domain, lease issuance, dispatch IDs, revocation, cleanup
                              |
          +-------------------+-------------------+
          |                                       |
 simulator (ineligible for VM)     Virtualization.framework (fail-closed unless eligible)
          |                                       |
          +-------------------+-------------------+
                              v
          redacted projection (no frame bytes, paths, clipboard,
          credentials, network identities, helper secrets)
```

Adversarial cases covered by the executable matrix:

- forged guest/surface/domain/lease/frame/dispatch identity
- two-agent same-domain contention (one live lease per guest and per WorkAttempt)
- two isolated domains in parallel
- stale frame (zero backend input)
- duplicate dispatch_id (exactly-once)
- crash before/after input, then two restarts
- corrupted/legacy records quarantined
- source resolver traversal/symlink/rename/object substitution
- incomplete cleanup remains uncertain
- public projection secret/path needles

Never used: global mouse/keyboard injection, `CGEvent`, AppleScript, clipboard,
credential UI, permission prompts, browser auth, or unrelated apps.

## Lifecycle

Live phases: `create → ready → running → closing`.

Terminal truth: `failed | interrupted | quarantined`. Restart interrupts the
guest, issues a new surface incarnation, revokes live leases, and converts
`Injected` receipts to `uncertain`. Old incarnations are never resumed.

## Budgets

Throughput (`max_frames`, `max_input_events`, `duration_seconds`) is terminal.
Throughput bytes (`max_captured_bytes`) degrade capture and do not, by
default, terminate the Computer Run. Resident bytes decrement on frame
rotation and must drop to zero after cleanup.

## Qualification labels

| Evidence | VM qualification |
|---|---|
| Simulator host tests | ineligible |
| Source compilation / hermetic resolver | ineligible |
| `grokptah-isolated-visual-qualify` without VF launch | fail-closed / partial |
| Signed helper + image + VF boot/frame/input/cleanup | required for PASS |

## Continuous integration

`.github/workflows/computer-use-isolated-visual.yml` is the only place this
crate's tests execute. `grokptah-isolated-visual` declares its own
`[workspace]`, so the `grokptah-agent-bridge` and desktop `cargo test`
invocations compile the library as a path dependency but never run its 23 unit
tests or the 5-case adversarial matrix.

| Job | Runner | Proves |
|---|---|---|
| `isolated-visual` | ubuntu-latest | fmt, clippy `-D warnings`, serial tests, adversarial reachability, fail-closed qualification |
| `lockfile-integrity` | ubuntu-latest | every committed `Cargo.lock` resolves under `--locked` |

Tests run with `--test-threads=1`. That is required, not stylistic: the store
takes advisory file locks and `store_lock_rejects_a_second_open` asserts a
second open is refused, which only holds when nothing else holds the lock.

Two checks back this, and both are runnable locally:

```sh
scripts/check-committed-lockfiles.sh     # every committed lockfile resolves
scripts/check-adversarial-reachable.sh   # the adversarial suites still run
```

`check-adversarial-reachable.sh` labels its evidence. `[dynamic]` means the
test binary was compiled and the harness enumerated the name; `[static]` means
only the file and its invocation path were inspected. The bridge suites
(`orchestration_adversarial`, `computer_use_release_gate`,
`isolation_capability`) are `[static]` here because they run under
`desktop.yml` on macOS, not in this workflow.

CI runners have no Virtualization.framework and no signed helper or image, so
the qualification step asserts the evidence stays `fail_closed` and never
reports a launch. A green run is simulator and source evidence only: it is not
VM, macOS, or hardware qualification, and the table above still governs.

## Continuation (blocked on disk/artifacts)

This environment had less than 25 GiB free, occupied Cargo targets, and no
signed helper/image. Do not create a guest image or a broad `grokptah-agent-bridge`
target until capacity is clear.

```sh
cd /private/tmp/grokptah-cu-isolated-vm-lifecycle-v1
CARGO_TARGET_DIR=/tmp/grokptah-isolated-visual-target \
  cargo test --locked --manifest-path crates/codegen/grokptah-isolated-visual/Cargo.toml
CARGO_TARGET_DIR=/tmp/grokptah-isolated-visual-target \
  cargo run --locked --manifest-path crates/codegen/grokptah-isolated-visual/Cargo.toml \
  --bin grokptah-isolated-visual-qualify -- --out /tmp/isolated-visual-evidence.json
```

Real VF boot is allowed only after `IsolatedPreflight.allowed_to_launch` is
true on the exact head.
