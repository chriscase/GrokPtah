# PR #399 — exact-head qualification review and repair handoff

Independent review. Source/CI evidence only. **No qualification claim is made here.**

## 1. Source gate (verified, not substituted)

| Role | SHA | Verified |
|---|---|---|
| PR #399 head | `fccb7fc58aa7d0727c4daa344a3d78966fabefbd` | `git cat-file -t` = commit |
| Parent of head | `bd7a2e11b09d310689f127144d400e2997750c58` | matches `%P` of head |
| Base (`cursor/ci-374-clippy-reexport-guest-tmp-acbc`, draft #378) | `520d228d79ca7b0428426809cf195ddf493c3623` | commit |
| CI-assembled merge (`refs/pull/399/merge`) | `11f432460343c2c51e16a5882945343bc631c7da` | parents = `520d228` + `fccb7fc` |

`main` (`67e29bd3…`) was **not** substituted at any point. Work was done in a detached
worktree pinned to `fccb7fc`.

## 2. Verdict on `shared_black_box_v1_desktop_hosted_parity`

**Stale candidate / CI topology. Not an authorized assembled-head golden update, and
not demonstrable as a semantic mismatch.**

### 2.1 Exact failure

```
tests/common/shared_black_box_v1.rs:2733:34
unexpected source revision 11f432460343c2c51e16a5882945343bc631c7da;
fail closed (no golden inference by feature downgrade)
```

`11f4324…` is **not a source revision**. It is `refs/pull/399/merge` — the ephemeral
merge commit GitHub assembles per PR, on no branch, regenerated whenever either side
moves. Both committed goldens are keyed to real **single-parent ancestor** commits:

| Audited revision | Golden | Shape |
|---|---|---|
| `4bd2081b2945e8ce881895f976bb7c8d88b929f2` | `expected-pr352-4bd2081b.json` | single parent, ancestor of head |
| `67e29bd34dc64049432c715c93c2cef2185c63ea` | `expected-main-67e29bd3.json` | single parent (main tip), ancestor of head |

### 2.2 Why the harness picks the merge SHA — two compounding defects

Both originate at `.github/workflows/hosted-service.yml:42`, a bare
`- uses: actions/checkout@…` with no `with:` block.

1. **No `ref:`** → on `pull_request`, checkout resolves `refs/pull/N/merge`, so
   `git rev-parse HEAD` is the assembled merge, never the reviewed head.
2. **Default `fetch-depth: 1`** → the clone is shallow. Reproduced exactly:

   ```
   .git/shallow present
   git rev-parse HEAD    -> 11f432460343c2c51e16a5882945343bc631c7da
   git rev-parse HEAD^   -> fatal: ambiguous argument 'HEAD^': unknown revision
   ```

   `commit_changed_files()` therefore takes its `else` branch
   (`shared_black_box_v1.rs:2938-2945`) and runs
   `git diff-tree --no-commit-id --name-only --root -r HEAD`, which on a shallow
   boundary emits **the entire tree — 3333 files**. `any(|p| !allowlisted(p))` is
   true on iteration one, so `detect_audited_source_revision()` returns HEAD
   immediately.

The consequence is structural: the parent walk and `FIXTURE_ALLOWLIST` skip — the
mechanism the whole golden scheme depends on, and the reason
`expected-pr352-4bd2081b.json` resolves correctly — **can never execute under this
checkout**. The design assumes a non-merge checkout with history.

### 2.3 Independent Cloud reproduction (decisive)

Same commit, same toolchain (rust 1.92.0), same OS family as the hosted job (ubuntu),
but a **non-merge, full-history** checkout at `fccb7fc`:

```
tests/common/shared_black_box_v1.rs:2733:34
unexpected source revision fccb7fc58aa7d0727c4daa344a3d78966fabefbd;
fail closed (no golden inference by feature downgrade)
```

Same panic site, **different revision identity**. This proves the reported revision is
a pure function of checkout topology, and that `11f4324` is a CI artifact rather than
the audited source revision of this change. The other 13 tests in the binary passed,
including `unknown_source_revision_fails_closed`,
`update_env_cannot_rewrite_or_bypass`, `missing_golden_fails_before_launch`, and
`expected_main_golden_is_immutable_for_audited_revision`; the worktree was clean
afterwards (no golden mutation). **The oracle's own fences are intact.**

### 2.4 No golden may be added for `11f4324`

Recording a golden keyed to the assembled merge would pin the oracle to a
non-audited, ephemeral artifact that changes on every push to either branch — exactly
the inference the fence at `select_golden_file()` exists to prevent. Do not do it.

### 2.5 Fixing checkout does not turn this job green

Per §2.3, a correct checkout changes the failure to `unexpected source revision
fccb7fc…`. That is the **honest** fail-closed state: #399 changes product source and
has no audited golden for its head. Closing it requires an authorized audited
recording, which cannot be produced from source-only evidence and is **not** performed
here.

### 2.6 No semantic evidence exists either way

`run_fixture()` panics at `select_golden_file()` (line 318), **before** `run_v1(Desktop)`
and `run_v1(Hosted)` (lines 337-338). No behavior was executed, no desktop/hosted
comparison, no redaction scan, no golden content comparison. Any claim of parity **or**
of semantic mismatch would be unfounded.

## 3. Repair sequence (ordered; step 1 is a prerequisite for step 2)

**Step 1 — make the audited revision identifiable.** `.github/workflows/hosted-service.yml:42`:

```yaml
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          ref: ${{ github.event.pull_request.head.sha || github.sha }}
          fetch-depth: 0
```

`ref:` stops the assembled merge from being mistaken for a source revision;
`fetch-depth: 0` restores the history the parent walk and allowlist skip require.
This is a CI-topology correction and **weakens no fence** — the job still fails closed.
It is outside #399's declared changed-file allowlist and belongs in its own change.

**Step 2 — authorized golden recording for `fccb7fc` (requires authority; not done here).**
Only after step 1. Per the recording note at `shared_black_box_v1.rs:96-98`, flip
`PRELOAD_IMMUTABLE_GOLDEN` to `false` locally, dump the normalized JSON to temp,
review it, then restore `true` before push. Then add the entry to **both**
`AUDITED_GOLDENS` (`shared_black_box_v1.rs:67`) and `scenario.json`'s `goldenSelector`
— `select_golden_file()` panics if they disagree — and add the filename to
`FIXTURE_ALLOWLIST`. Because all three files are allowlisted, the recording commit is
skipped by the walk and the audited revision still resolves to `fccb7fc`.

Verify with:

```
cd crates/codegen/grokptah-service
cargo test --locked --test shared_black_box_v1 -- --test-threads=1
```

## 4. Desktop job — 5 failures, 750 passed

The PR body presents desktop as corrected via the `SemanticElement` re-export. That
change did land, and its effect was to make the lib compile — which caused
`cargo test --lib` to actually run for the first time and **reveal** five pre-existing
failures. Attribution against `520d228..fccb7fc`:

| # | Test | Site | Root cause | Changed by #399? | Linux repro |
|---|---|---|---|---|---|
| 1 | `isolated_visual::…::serialized_contract_contains_no_host_paths_or_channel_secret` | `isolated_visual.rs:643` | Over-broad needle. Forbidden list uses bare `"credential"`; `IsolatedVisualSecurityProfile.credential_forwarding: false` serializes (camelCase) as `"credentialForwarding"`, which contains it. The match is on the field name that *proves* forwarding is disabled — **no secret leaks**. | No — file untouched | Yes, identical |
| 2 | `isolated_visual_channel::…::canonical_binding_vector_matches_freestanding_guest` | `isolated_visual_channel.rs:315` | Test arithmetic. `"domain-1"` is 8 bytes but the expectation adds 7: `80 + 5 + 9 + 13 + 8 = 115` vs `… + 7 = 114`. The pinned digest assertion just above **passes**, so the canonical binding is correct. Fix: `+ 7` → `+ 8`. | No — file untouched | Yes, `left: 115 right: 114` identical |
| 3 | `isolated_visual_runtime::…::runtime_requires_binding_before_channels_and_stop` | `isolated_visual_runtime.rs:313` | Test data. Nonce `"00112233445566778899aabbccddeeff"` is undashed, and its version nibble is `6`. `validate_request_nonce` → `parse_uuid` (`isolated_visual_frames.rs:478-491`) requires `uuid.to_string() == value` (canonical hyphenated) and then UUIDv4 — doubly invalid. Fix: use a canonical hyphenated v4 literal. | File yes, **failing test body no** | Yes, identical |
| 4 | `isolated_visual_artifacts::…::writable_handles_wrong_modes_and_sparse_oversize_fail_closed` | `isolated_visual_artifacts.rs:533` | Setup contradicts intent. The test needs a **writable** handle to prove writable handles are rejected, but `write_artifact("helper", …, executable = true)` chmods the file to `0o500` (`:498`) — no write bit — so `OpenOptions::read(true).write(true).open()` returns `EACCES` for any non-root user. Fix: acquire the writable handle before the chmod, or use `0o700` for this case. | No — file untouched | Passes here only because the container runs as **root** (root bypasses the DAC write check); not a macOS-specific defect |
| 5 | `macos_observation::…::secure_values_are_removed_and_evidence_is_exactly_scoped` | `macos_observation.rs:2525` | `observation.elements.len()` is 2, expected 1 — an element the scoping should drop is surfacing. Needs macOS to diagnose. #399's only edit to this file is a `drop(requests)` → block-scope refactor at ~2119-2137, in a **different** test. | File yes, **failing test body no** | macOS-gated |

**None of the five is introduced by #399's diff.** They belong to the base stack
(#378/#374) and should be repaired there, not by widening #399's allowlist. #1–#3
reproduce deterministically on Linux, so they are source-level defects, not runner
flakes. #5 is the only one that is potentially a genuine redaction/scoping regression
and it is the one that still needs a macOS diagnosis — it should not be dismissed.

Reproduce #1–#3 without macOS:

```
cd crates/codegen/grokptah-agent-bridge
cargo test --locked --lib computer_use::isolated_visual -- --test-threads=1
```

## 5. Always-On probe — correct fail-closed, not a regression

`always-on-grokbot` exits 3 with
`{"certified":false,"failure_classes":["configuration"],"summary":{"indeterminate":1,…}}`.

`always-on-grokbot-lifecycle-v1` is declared at `evals/certification-lab/campaign.v1.json:2344`
and invoked at `always-on-grokbot-cert.yml:55`, but has **no** arm in
`implementation_tools()` (`probes.rs:307`) and **no** dispatch arm, so it falls to
`_ => Err(DiagnosticCode::ProbeImplementationUnavailable)` (`probes.rs:543`).

Reporting an unimplemented probe as `indeterminate`/`configuration` rather than passed
is the correct fail-closed behavior, and matches the PR body's own disclosure. The
job cannot go green until the probe is implemented. The 22 process-level Always-On
tests pass (`22 passed; 0 failed; 3 ignored`).

## 6. Authority finding attributable to #399 (needs an author decision)

`crates/codegen/grokptah-agent-bridge/src/computer_use/isolated_visual_runtime.rs`,
on `IsolatedVisualRuntimeSession` — which was already `pub` and already re-exported at
the crate root before this PR (`computer_use/mod.rs:101`), so both changes land on the
**public API**:

- `fail()` widened `pub(crate)` → `pub`.
- New `pub fn complete_observed_cleanup(&mut self, helper_process_absent, no_open_handles, overlay_removed, frame_cache_removed)`.

Its own doc comment states: *"Evidence construction stays crate-private so a
coordinator cannot manufacture terminal authority from serialized booleans."*
`IsolatedVisualCleanupEvidence::verified` is indeed `pub(crate)`
(`isolated_visual.rs:251`), but this new **public** wrapper on a crate-root-re-exported
type constructs that evidence from four caller-supplied booleans, so the stated
invariant no longer constrains external callers.

What still holds: `validates_for()` (`isolated_visual.rs:270-291`) fails closed on any
`false` (`Conflict`) and binds the evidence to the session's own surface
(`ForbiddenTarget` otherwise). So no cross-surface completion and no negative-fact
completion. **Residual risk:** an external caller can *assert* cleanup that was never
observed and thereby complete terminal lifecycle authority.

By contrast the packaged-runtime fence in the PR body holds as written:
`IsolatedVisualPackagedRuntime` is `pub(crate)` (`macos_isolated_runtime.rs:184`) and is
**not** re-exported at the crate root. Only `SemanticElement` was added to the crate-root
re-export list. The `isolated_visual_protocol.rs` change is a semantically identical
clippy fix (`% 2 != 0` → `!is_multiple_of(2)`).

Recommend confirming the widening is intentional, or narrowing it to the internal
driver chain (`isolated_visual_driver.rs:175` → `macos_isolated_runtime.rs:370`), which
is its only current consumer.

## 7. Not claimed

No qualification, Stage 5/6, or 100% claim. Not established here: signed helper/image,
Virtualization.framework launch, real guest boot/frames/input, live cleanup, hardware
matrix, soak evidence, desktop/hosted semantic parity. All findings are source and CI
evidence at the exact head. No golden was added, weakened, inferred, or bypassed; no
fail-closed path was relaxed. #399, #378, and #374 were left untouched — not merged,
undrafted, retargeted, or published.
