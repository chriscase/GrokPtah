# Isolated Visual Computer Use — donor matrix

Reconstruction base: **`6c1c4c3cd8d0398f1d673a04d6187c6e60780780`** (exact head of PR #424,
branch `codex/pr423-continuity-repair-v1`, itself stacked on `712f41be6532b085aefa1244afbfd015726d8e48`).
`main` was never substituted for this head, and no donor branch was merged, rebased, or
cherry-picked into it.

The alternate history rooted at `295a4ff` is used **read-only**. Every admitted item below was
read out of a donor tree and re-applied by hand onto the #424 head, adapted to the types that
exist at that head. `git merge`, `git rebase`, `git cherry-pick`, and `git checkout <donor> -- …`
of whole donor trees were not used for any admitted item.

## Topology

| History | Commit | Role |
| --- | --- | --- |
| Reconstruction base | `6c1c4c3cd8d0398f1d673a04d6187c6e60780780` | PR #424 head; all #424 proofs preserved byte-identical |
| Base of #424 | `712f41be6532b085aefa1244afbfd015726d8e48` | PR #423 head |
| Common ancestor | `127ffaff78b230dff7334ad692c382b66d1d1287` | merge-base(#424 head, donor history) |
| Donor root | `295a4ff62939af1a3034119653c83c7a0a2e1bff` | carries the isolated_visual / helper / guest source and runbooks |

The two histories diverged 141 commits (#424 side) / 436 commits (donor side) after
`127ffaf`. The donor `computer_use` tree at `748ab12` carries an authority spine
(`ComputerAuthorityToken`, `ComputerPrincipal`, `ComputerSurfaceEvent`, `ComputerCapabilityProof`, …)
that does **not** exist at the #424 head. That spine is **out of scope** here: it is not part of the
isolated visual substrate and is not reconstructed. Only the substrate's own two missing
supporting types are added, and both are crate-private.

## Admitted donors

Each row records the exact SHA, the semantics admitted, and the hunks deliberately left behind.

| # | Donor SHA | Admitted semantics | Files touched (admitted hunks) | Explicitly not taken | Milestone |
| --- | --- | --- | --- | --- | --- |
| 1 | `295a4ff62939af1a3034119653c83c7a0a2e1bff` | `isolated_visual*` substrate source, packaged helper source, guest source, runbook | `src/computer_use/isolated_visual{,_artifacts,_channel,_driver,_frames,_helper,_helper_control,_input,_input_wire,_protocol,_runtime,_stream}.rs`; `src/computer_use/macos_isolated_{artifacts,runtime}.rs`; `desktop/src-tauri/macos/isolated-visual-guest/*`; `desktop/src-tauri/macos/isolated-visual-helper/*`; `docs/COMPUTER_USE_ISOLATED_VISUAL.md` | `src/computer_use/coordination.rs` (surface-coordination feature, not substrate); donor `Cargo.lock`; `.github/workflows/isolated-visual-guest.yml`; donor evidence/qualification docs | M1–M3 |
| 2 | `5919e3343af20a78e17459b8ac8454bbc5aeca7e` (PR #374 head) | closed guest lifecycle phases; one-agent revisioned lease with expiry/revocation; capture redaction | adds `src/computer_use/isolated_guest.rs`; `isolated_visual.rs` (+29), `isolated_visual_channel.rs`, `isolated_visual_driver.rs`, `isolated_visual_frames.rs`, `isolated_visual_input_wire.rs`, `isolated_visual_runtime.rs` | none in substrate scope | M1 |
| 3 | `5127d3f2fd7d80cf1e18c2919473bdea7e951343` | Clippy / test / temp-verifier fixes only | `desktop/src-tauri/macos/isolated-visual-guest/verify-guest-source.sh` (temp handling) | **`src/lib.rs` crate-root re-export hunk — denied**; `grokptah-service/tests/always_on_grokbot.rs`; `tests/common/shared_black_box_v1.rs` | M2 |
| 4 | `520d228d79ca7b0428426809cf195ddf493c3623` (PR #378 head) | freestanding guest-init fix (PID 1 must not emit `memset`) | `guest-init.c` (+29), `verify-guest-source.sh` (+48) | none in substrate scope | M2 |
| 5 | `203a5cf3fa785c2010b34be9e154b7080c491775` | packaged helper / runtime / protocol / frame / input wiring hunks only | `isolated_visual_driver.rs`, `isolated_visual_input_wire.rs`, `isolated_visual_protocol.rs`, `isolated_visual_runtime.rs`, `build-guest-image.sh`, `kernel.config.fragment`, `verify-guest-source.sh` | "Always-On operator primary" hunks (unrelated orchestration) | M1–M3 |
| 6 | `811ece3d009c8657b0c5091a1e87e541306ba101` | keep `SurfaceAuditInput` crate-private to the audit test | `src/computer_use/projection.rs` (2 lines) | none | M5 |
| 7 | `e98a6a98a92814a2d76c1ff0fd033b786dd04cd7` | crate-private packaged runtime; INITRAMFS-stays-in-kernel-tree corrections | `macos_isolated_runtime.rs`, `computer_use/mod.rs` (privacy), `build-guest-image.sh`, `verify-guest-source.sh` | `src/lib.rs` hunk (crate-root surface); `always_on_grokbot.rs` | M2–M3 |
| 8 | `a26f42085334b4716f1dc3d58287a5b83242d773` | accept the `# CONFIG_X is not set` disabled spelling after `olddefconfig` | `build-guest-image.sh` (+26), `verify-guest-source.sh` | none | M2 |
| 9 | `94e9437e2c45022ff442092866b5eea881d2396d` | encoding + fixture alignment in substrate unit tests | `isolated_visual.rs`, `isolated_visual_artifacts.rs`, `isolated_visual_channel.rs`, `isolated_visual_runtime.rs` | none | M1–M2 |
| 10 | `a57057790979d2e9b691ae5086d4de86c3df2877` | sample background pointer state **without** Quartz `CGEvent` APIs | `src/computer_use/macos_native.rs`, `src/computer_use/macos_native_shim.m` | none | M5 |
| 11 | `097301de1d612696afd079ad6e28705427f85fab` (PR #399 head) | fail-closed: ungranted remote bearer is a Computer-read denial | `tests/mcp_streamable_transport.rs` | none | M5 |
| 12 | `748ab129b0b06c2fb475990f8f572c93ac87d392` (PR #404 parent of head) | packaged supervisor binding; direct (non-crate-root) imports; guest matcher self-test | `isolated_visual.rs` (+23 supervisor binding), `macos_isolated_runtime.rs`, `build-guest-image.sh`, `verify-guest-source.sh` (absent-key self-test) | `.github/workflows/hosted-service.yml`; `tests/common/shared_black_box_v1.rs`; `evals/certification-lab/*` | M2–M3 |
| 13 | `3e6bde2c13bd26aae9494843ac52c92744072fec` | matching-tree merge-identity detector and its tests only | detector logic + tests | `.github/workflows/hosted-service.yml` hunk | M5 |

## Denied donors

| Denied | SHA / ref | Reason |
| --- | --- | --- |
| Crate-root re-export of `SemanticElement` | `fccb7fc58aa7d0727c4daa344a3d78966fabefbd` | old public re-export; substrate must stay crate-private |
| Crate-root re-export hunk of `5127d3f2` | `5127d3f2…` `src/lib.rs` | same |
| Head-specific parity golden (PR #410 head) | `8c68157ffbf876ebf1ddb3f42386196effd1a0b0` | head-specific golden |
| PR #404 head | `4f87e71613431882e4d1ef0bafa3e20471f449e8` | maps a donor-head-specific shared black-box golden |
| Managed Agent-failed / principalId scan | `c5003a4ee815e7c8b53de67d777beb9a6cc467ca` | unrelated orchestration |
| Always-On + Stage 6 oracle alignment | `acc51e2d9c590e3aa9deb3161a68381d586e4a25` | unrelated certification |
| Replay / token-accounting terminals | `2bb0a111d38a97e791feea25874014273c7e311e` | unrelated orchestration |
| `#[used]` lib-artifact linkage | `bd7a2e11b09d310689f127144d400e2997750c58` | not in the admitted set; superseded by #12's binding. Its `to_message` → `into_message` rename is **not** imported as a donor hunk — the consuming-method name is independently required by `clippy::wrong_self_convention` under the strict gate |
| PR #409, PR #408 broad history | — | out of scope |
| PR #425 / #426 / #427 | `8827be56…`, `58bff58a…`, `a5cd3366…` | explicitly excluded from this reconstruction |
| Donor lockfiles | donor `Cargo.lock` (bridge + desktop) | head lock is regenerated by cargo on the #424 head, never copied |
| `docs/COMPUTER_USE_ISOLATED_RUNTIME_EVIDENCE.md`, `docs/verify-isolated-runtime-evidence.sh` | `295a4ff` | old evidence hashes |
| `docs/COMPUTER_USE_ISOLATED_QUALIFICATION_RUNBOOK.md` | `295a4ff` | carries qualification claims; a fresh non-claiming runbook is authored instead |
| `docs/COMPUTER_USE_ISOLATED_GROK_BUILD_HANDOFF.md` | `295a4ff` | unrelated orchestration handoff |
| `.github/workflows/isolated-visual-guest.yml` | `295a4ff` | Actions are not mutated by this reconstruction |
| `src/computer_use/coordination.rs` | `295a4ff` | separate surface-coordination feature, not the isolated visual substrate |

## Applicability at this base

Three admitted donors turned out to have nothing to apply at `6c1c4c3`, because what they
repair does not exist on this side of the divergence. They are recorded here rather than
quietly dropped, and in each case the property they protect is either already held or is
carried instead by a gate written against this base's API.

| Donor | Why it does not apply | What holds the property here |
| --- | --- | --- |
| `811ece3d` (private `SurfaceAuditInput`) | `SurfaceAuditInput` is part of the donor's authority spine (`ComputerSurfaceEvent`, `ComputerAttentionPoint`, …) and does not exist at `6c1c4c3`, so there is no import to move | The lint discipline it encodes — a `pub(crate)` item used only by a test is imported in the test, not the lib — is what the whole reconstruction follows, and the lib's dead-code count is held at the untouched head's baseline |
| `3e6bde2` (matching-tree merge detector) | Its only in-scope target is `tests/common/shared_black_box_v1.rs`, which does not exist at this base; its other hunk is a workflow change, which is denied | Nothing at this base infers a golden from a merge SHA, so there is no detector to harden |
| `097301de` (fail-closed ungranted remote bearer) | The `AuthCredential::with_computer_read_grant` fixture and the `remote_bearer_computer_reads_fail_closed` test it repairs are both donor-side; this base has neither, and its Computer-read surface is already gated by `ComputerReadBinding` with its own scoping tests | The base-appropriate form of "ungranted means denied" is enforced for this substrate by the gate asserting the isolated runtime has **no public surface at all**: there is no entrypoint an ungranted caller could reach |
| `a5705779` (pointer sampling without Quartz `CGEvent`) | Partially applies. Its shim change edits `GPTCaptureUserInteractionState`, which this reconstruction deliberately does not import, because that sampler exists to serve the denied measured-background act path | Its actual security property is preserved and strengthened: this base already forbids `CGEventCreate`, the M3 shim additions introduce no `CGEvent` use at all, and a new gate enforces the whole Quartz sampling and injection family on **every** platform rather than only on macOS |

## Dependency delta

`ring` is the donor's HMAC primitive for the channel binding, frame carrier, input wire, and
protocol envelope. It is **already present in the #424 head lockfile** at exactly
`0.17.14`, checksum `a4689e6c2294d81e88dc6261c768b63bc4fcdb852be6d1352498b114f61383b7` —
identical to the donor lock — pulled in transitively by `rustls`. Promoting it to a direct
dependency of `grokptah-agent-bridge` therefore adds a dependency **edge** only: no new package,
no version change, no checksum change, and no donor lockfile is copied. The version is pinned,
not floating.

`chrono-tz` and `grokptah-test-gateway` appear in the donor manifest for unrelated features and
are **not** taken.

## Supporting types added at the head

The substrate's only unmet type dependencies at `6c1c4c3` are `ComputerSurfaceBinding` and
`PointerButtonState`. Both are added to `src/computer_use/types.rs` as **crate-private**
(`pub(crate)`) items and are not re-exported from `computer_use` or the crate root.

## Preserved #424 proofs

The following are carried forward from `6c1c4c3` **unchanged**; no reconstruction commit edits them:

- durable gap-recovery continuity (`cursorExpired` on a fully evicted retained prefix);
- the stable public transport error envelope (`code` + `reasonCode`);
- the max-cursor bound;
- the hidden-persistence-failure rejection.
