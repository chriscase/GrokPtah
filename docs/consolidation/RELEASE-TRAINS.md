# GrokPtah PR / branch consolidation map and main-promotion plan

**Anchor:** `67e29bd34dc64049432c715c93c2cef2185c63ea` — verified byte-equal to `origin/main` at analysis time (*Manager v2: autonomous durable coordination (#339)*, 2026-08-21T15:31:45-07:00). Analysis fails closed if that object is absent; it was present.

**Generated:** 2026-08-29T14:20:34.577039+00:00 · **Machine-readable companion:** `docs/consolidation/pr-inventory.json` (129 PR records)

**Nothing was mutated.** No merge, rebase, force-push to a PR branch, close, undraft, retarget, branch deletion, worktree deletion, or test change was performed. All merge results below come from `git merge-tree --write-tree` (object-only, non-destructive); train states were materialised with `git commit-tree` and never written to a ref that leaves this branch.

---

## 1. Exact counts

| Measure | Count |
|---|---|
| Open pull requests | 129 |
| PR number range | #340 – #490 |
| Draft | 128 |
| Ready for review | 1 (only #376) |
| Target `main` directly | 53 |
| Stacked on another branch | 76 |
| Stacked on a branch that has **no open PR** | 17 |
| Fork from current main `67e29bd` | 83 |
| Fork from stale `127ffaff` | 46 |
| Merge **clean** into `67e29bd` | 83 |
| Merge **conflicts** with `67e29bd` | 46 |
| Superseded by exact git ancestry | 52 |
| PR body cites a non-ancestor SHA | 20 |
| Content already byte-identical in main | 0 |
| Hosted check observed / green / red | 33 / 26 / 5 |

**Dispositions**

| Disposition | Count |
|---|---|
| KEEP WHOLE | 18 |
| SELECTIVE DONOR | 7 |
| SUPERSEDED | 52 |
| REWRITE | 25 |
| NEEDS QUALIFICATION | 27 |

> `REJECT` is folded into `SUPERSEDED` for #416/#417 (identical commits) — see F5.

---

## 2. Structural findings

### F1 — 46 of 129 open PRs fork from 127ffaff78b230dff7334ad692c382b66d1d1287, not from current main.  
*Severity: critical*

**Evidence.** git merge-base(head, 67e29bd) == 127ffaff for exactly those 46. Main carries 72 commits after 127ffaff touching 242 files.

**Consequence.** All 46 conflict against main (111-116 conflicted paths each). The CONFLICT set and the stale-fork set are the same 46 PRs, exactly.

### F2 — codex/external-worker-hardening-v1 is a 132-commit / 293-file branch that has never had a pull request, yet 15 open PRs are based on it.  
*Severity: critical*

**Evidence.** git ls-remote tip 8ad3be07eb27087acb67704fdf463ecb95b64505; merge-base with main 127ffaff; no ref under refs/pull/*/head resolves to it; not an ancestor of main.

**Consequence.** Those 15 PRs' diffs are measured against unreviewed content. Their GitHub 'files changed' understates what reaching main would introduce.

### F3 — Five open PRs independently define a competing host-authority type; they mutually conflict.  
*Severity: high*

**Evidence.** #460 and #474 each define AuthEpoch; #470 defines VerifiedPrincipal; #472/#486/#490 define CapabilityGeneration; #489 defines AuthEpoch+PrincipalScope+VerifiedPrincipal; #488 is a second full G1-G4 spine. Pairwise merge-tree: #488x#489 conflict on 7 files, #460x#489 on 8, #486x#488 on 7.

**Consequence.** Exactly the multi-identity-fence hazard issue #477 was opened to prevent. At most one may be promoted whole.

### F4 — Both canonical-spine candidates are red.  
*Severity: high*

**Evidence.** #489 desktop=failure (mergeable_state unstable); #488 desktop=failure (mergeable_state unstable).

**Consequence.** No authority spine is currently promotable. The green authority-adjacent PRs (#482, #486) are capability-generation only, not the full spine.

### F5 — #416 and #417 are the same commit.  
*Severity: medium*

**Evidence.** Both heads are 33b74ce6a7f4446303adbcb07dddd2913473108f with identical tree 0b7b1b03c0f1. Branches claude/external-worker-gate-4-zj3elc and claude/external-worker-gate-5-prod-surface point at the same object.

**Consequence.** One is redundant.

### F6 — The hosted gate is a single job for most PRs.  
*Severity: medium*

**Evidence.** Of 34 PRs with observed checks, 31 have exactly one check run named 'desktop'. Ten workflows exist in main but only 'desktop' runs on most PRs.

**Consequence.** Green CI here is weak evidence; it does not exercise packaged, live-provider, or isolated-VM claims.

### F7 — 20 open PRs cite a commit SHA in their body that is not an ancestor of their current head.  
*Severity: medium*

**Evidence.** Contextual SHA extraction ('head/commit/base/at <sha>'), Actions run-ids excluded, resolved via git rev-parse against the fetched object store.

**Consequence.** PR bodies cannot be trusted as provenance without re-derivation.

### F8 — No open PR contains content already byte-identical in main.  
*Severity: low*

**Evidence.** For all 129, files_vs_main_tip is non-empty and head_is_ancestor_of_main is false; zero pure no-ops.

**Consequence.** No PR can be closed on grounds of already being merged.

---

## 3. Supersession map (exact git ancestry, not heuristic)

Each row: the older PR's head **is a git ancestor of** the newer PR's head, so the newer head already contains it byte-for-byte.

| Superseded | Head | Contained by | Domain |
|---|---|---|---|
| #340 | `b3835d9515c6` | #360, #367, #374, #375, #376, #377, #378, #379, #399, #404, #408, #409, #410, #414, #418, #439 | desktop-ui |
| #343 | `b67c3ecef93b` | #350, #351, #352, #354, #358, #365, #371, #373, #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | orchestration |
| #350 | `78c9c07c3076` | #351, #352, #354, #358, #365, #371, #373, #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | orchestration |
| #351 | `cf04ceb60f8a` | #352, #354, #358, #365, #371, #373, #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | orchestration |
| #352 | `4bd2081b2945` | #354, #358, #365, #371, #373, #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | agent-sdk |
| #353 | `e5828740e1bb` | #357, #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | computer-use |
| #354 | `1ab099623d07` | #371, #373, #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | agent-sdk |
| #355 | `6c5cd6bf1732` | #366, #368, #372, #374, #376, #378, #399, #401, #404, #408, #409, #410, #414, #418, #439 | computer-use |
| #356 | `6da3561e3856` | #370, #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | enterprise-gateway |
| #360 | `4cd7f8667588` | #367, #374, #375, #376, #377, #378, #379, #399, #404, #408, #409, #410, #414, #418, #439 | desktop-ui |
| #361 | `d0f6ff517095` | #370, #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | enterprise-gateway |
| #362 | `0ba0a57c315d` | #369, #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | enterprise-gateway |
| #366 | `f481c46d296e` | #368, #372, #374, #376, #378, #399, #401, #404, #408, #409, #410, #414, #418, #439 | computer-use |
| #367 | `5d5aca54b9c0` | #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | desktop-ui |
| #368 | `7239201e9701` | #372, #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | computer-use |
| #370 | `72540ec0a43a` | #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | enterprise-gateway |
| #371 | `4f429d86a0c0` | #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | agent-sdk |
| #372 | `059708975d75` | #374, #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | computer-use |
| #374 | `5919e3343af2` | #376, #378, #399, #404, #408, #409, #410, #414, #418, #439 | computer-use |
| #377 | `57b14da3855d` | #379 | desktop-ui |
| #378 | `520d228d79ca` | #399, #404, #408, #409, #410, #414, #418 | computer-use |
| #380 | `d2791b6c8751` | #392, #393, #395 | semantic-help |
| #381 | `faaf4074b1c7` | #394, #396, #397, #398, #416, #417 | computer-use |
| #382 | `aa70569b2c29` | #385 | computer-use |
| #384 | `72ea28175f29` | #386, #387, #388, #389, #390, #391, #400, #402, #403 | computer-use |
| #387 | `c103594129c6` | #391, #400, #402, #403 | computer-use |
| #388 | `60ecc2f2d800` | #389, #390 | computer-use |
| #391 | `c7f28d81652e` | #400, #402, #403 | computer-use |
| #392 | `9cab6195383e` | #393, #395 | semantic-help |
| #393 | `ffbd53c9fc18` | #395 | semantic-help |
| #394 | `a248596297d4` | #396, #397, #398 | computer-use |
| #397 | `2daf89ad8cce` | #398 | computer-use |
| #399 | `097301de1d61` | #408, #409, #410, #414, #418 | computer-use |
| #400 | `4f158d119f8f` | #402, #403 | computer-use |
| #402 | `f4a469759f42` | #403 | computer-use |
| #405 | `697c28bdc2e4` | #407 | computer-use |
| #411 | `d7bde3ce39b7` | #427, #454 | computer-use |
| #412 | `342a3bacca02` | #413, #419, #422 | computer-use |
| #413 | `56146e4d893f` | #419 | computer-use |
| #416 | `33b74ce6a7f4` | #417 | computer-use |
| #417 | `33b74ce6a7f4` | #416 | computer-use |
| #423 | `712f41be6532` | #424, #425, #426, #427, #428, #429, #430, #432, #454 | computer-use |
| #424 | `6c1c4c3cd8d0` | #428, #429, #430, #432 | computer-use |
| #425 | `8827be56ac4f` | #427, #454 | computer-use |
| #427 | `e616a2150ca2` | #454 | computer-use |
| #430 | `e732b5da3207` | #432 | computer-use |
| #431 | `19b84a64b322` | #471 | agent-sdk |
| #446 | `2274cc69d7f2` | #448 | docs-evals |
| #447 | `99489281a5f7` | #450 | computer-use |
| #459 | `5117f3218c34` | #469 | orchestration |
| #473 | `df21aa72fc45` | #472 | computer-use |
| #485 | `f083f52ea614` | #486 | agent-sdk |

**52 of 129 open PRs are strictly contained in another open PR.**

---

## 4. Ordered promotion graph

Seven trains, each rooted at `67e29bd`. Order **within** a train is load-bearing: it is the sequence that was simulated clean. Trains T1–T4 are blocked; T5–T7 are ready.

### T7 — Manager certification *(READY — simulated clean, builds clean)*

`#344 → #345 → #346 → #347 → #348`

| PR | Head | Files | Hosted check | Claim class | Title |
|---|---|---|---|---|---|
| #344 | `122a792e7c81` | 6 | success | source-only | Certify the manager plan lifecycle |
| #345 | `e1f92cbe14d8` | 1 | success | synthetic | Certify manager supervisor fairness and the autonomous gat |
| #346 | `af0c307f0512` | 1 | success | source-only | Parse manager directive envelopes strictly |
| #347 | `68a6dc2bde9a` | 1 | success | source-only | Certify manager tool scope enforcement |
| #348 | `41cba0cd680d` | 1 | success | source-only | Certify the privilege amplification guard |

- **Dependencies:** none. Forks from `67e29bd`; no PR in this train touches a file another train needs first.
- **Collision files:** none within the train. `#344` is confined to `evals/certification-lab/*` and its doc; `#345`–`#348` touch one file each.
- **Collides outside the train:** `evals/certification-lab/src/probes.rs` is also rewritten by `#414` (T9). Promote T7 first; `#414` must then rebase.
- **Required semantic review:** `#346` and `#348` change production code (`orchestration/manager.rs` +297, `orchestration/worker.rs` +185) — the directive-envelope parser and the privilege-amplification guard. These are authority-adjacent and must not be waved through on a green check.
- **Hosted check:** `desktop` green on all five.
- **Local gate:** `cd crates/codegen/grokptah-agent-bridge && cargo check --all-targets` then `cargo test --no-fail-fast`.

### T6 — Product UX *(READY — simulated clean)*

`#341 → #342 → #349`

| PR | Head | Files | Hosted check | Claim class | Title |
|---|---|---|---|---|---|
| #341 | `a1fd62aac013` | 3 | success | source-only | ToolCallCard: Tool Activity Evidence Center |
| #342 | `e55a21063b93` | 3 | success | packaged-claimed | Lane Context Command Header |
| #349 | `4437ba8a6dfa` | 5 | success | synthetic | Add Native Coding Readiness Center for provider qualificat |

- **Dependencies:** none.
- **Collision files:** none within the train; all three are disjoint desktop surfaces.
- **Required semantic review:** presentation-only; `#349` adds a *Readiness Center* that reports provider qualification — confirm it reports state rather than asserting qualification.
- **Hosted check:** `desktop` green on all three.

### T5 — Bounded runtime fixes *(READY — simulated clean)*

`#433 → #451 → #453 → #359 → #469 → #406`

| PR | Head | Files | Hosted check | Claim class | Title |
|---|---|---|---|---|---|
| #433 | `d96accd651b3` | 7 | success | source-only | Bind provider attempts to durable execution authority |
| #451 | `6218226dbc79` | 8 | success | source-only | fix(service): harden loopback probe boundary |
| #453 | `ca78af4f7c4b` | 12 | success | synthetic | feat(computer-use): enforce a strict model-output action b |
| #359 | `5f5b2713ff0d` | 10 | success | synthetic | feat: long-horizon durable memory core with logical-years  |
| #469 | `039bafb7a324` | 23 | success | packaged-claimed | feat(orchestration): run the shipped audit ledger on the v |
| #406 | `44a673b46440` | 19 | success | synthetic | fix: fence swarm control-plane authority |

- **Dependencies:** `#433` (bind provider attempts to durable execution authority) should land before `#469` (audit ledger) — both touch `orchestration/store.rs`, and `#433` establishes the attempt binding `#469` records.
- **Collision files (post-main, pairwise):** `#359 × #467` on `host.rs`; `#469 × #484` on 15 `audit/*` files; `#469 × #474` on `tests/orchestration_adversarial.rs`; `#453 × #482` on `computer_agent.rs`. None of those partners are in this train, so the train itself is clean — but promoting it **forces a rebase on #467, #474, #482, #484**.
- **Required semantic review:** `#453` enforces a strict model-output action contract for Computer Use — this is an effect boundary; review against issue #458's acceptance criteria before promotion. `#469` claims the shipped audit ledger runs on the real host: confirm the claim is scoped to what the test actually exercises.
- **Hosted check:** green on all six (`#406` via `crate`, the rest via `desktop`).

### T1 — Canonical G1–G4 authority / effect spine *(BLOCKED — no promotable head)*

This is the train issue #477 exists to produce, and **it currently has no green head.**

| PR | Head | Files | Hosted check | Defines | Title |
|---|---|---|---|---|---|
| #489 | `dea8339e1166` | 13 | failure | AuthEpoch+PrincipalScope+VerifiedPrincipal | authority: one host-issued principal and auth-genera |
| #488 | `adbfc79702fc` | 14 | failure | — | Canonical host authority spine rewrite (G1–G4) |
| #486 | `6ffdace0c906` | 23 | success | CapabilityGeneration | Repair durable authority boundaries for #477 (linked |
| #482 | `99eb2fc43856` | 21 | success | — | feat(bridge): add canonical provider capability-gene |
| #490 | `464045e34305` | 18 | success | CapabilityGeneration | computer-use: bind session authority to a provider c |
| #460 | `6be44a3cbd5b` | 10 | success | AuthEpoch | Stale-authentication epoch + principal ownership for |
| #474 | `ef1469c8c7ce` | 21 | success | AuthEpoch | [REWRITE — donor hunks only] session prompt-queue pr |
| #470 | `a847004a0839` | 8 | success | VerifiedPrincipal | feat(orchestration): reject dependency cycles, add r |
| #472 | `a97a8ab221f6` | 32 | none-observed | CapabilityGeneration | computer-use: durable, generation-bound adaptive pro |

**The conflict is real, not stylistic.** Measured pairwise conflicts after each is merged onto `67e29bd`:

| Pair | Conflicted files |
|---|---|
| #460 × #470 | 1 |
| #460 × #474 | 2 |
| #460 × #482 | 6 |
| #460 × #486 | 6 |
| #460 × #488 | 5 |
| #460 × #489 | 8 |
| #470 × #474 | 1 |
| #470 × #486 | 1 |
| #470 × #489 | 1 |
| #474 × #482 | 5 |
| #474 × #486 | 5 |
| #474 × #488 | 4 |
| #474 × #489 | 4 |
| #482 × #486 | 6 |
| #482 × #488 | 6 |
| #482 × #489 | 6 |
| #482 × #490 | 5 |
| #486 × #488 | 7 |
| #486 × #489 | 6 |
| #488 × #489 | 7 |
| #488 × #490 | 1 |

- **Dependency:** every other train that touches an effect boundary (T2 external worker, T3 adaptive Computer Use, T4 packaged Computer Use, enterprise gateway) is downstream of this one. Promoting them first re-encodes the identity fence they will later have to be rewritten onto.
- **Collision files:** `orchestration/authz.rs`, `orchestration/service.rs`, `orchestration/mod.rs`, `orchestration/store.rs`, `mcp_control.rs`, `lib.rs`, `host.rs`, `tests/native_executor_mcp.rs`.
- **Required semantic review:** the choice between `#489` and `#488` is an architecture decision, not a merge decision. `#489` matches issue #477's stated donor plan literally (KEEP #460's private epoch, KEEP only #474's `policy_revision`, REWRITE #470/#471 scopes, DROP #474's duplicate epoch) and defines all four canonical types under one private fence. `#488` is an independent spine that puts canonical authority on `OrchStore` and used `#486` as donor material only.
- **Hosted checks:** `#489` red, `#488` red. Both report `mergeable_state: unstable` (mergeable, checks failing).
- **Draft PRs that must stay open:** `#460`, `#470`, `#472`, `#474`, `#486`, `#488`, `#490` — each is the only place its donor hunks exist. Do not close any of them until the chosen spine is green on `main` **and** the specific hunks named in #489's donor table have been re-derived onto it.

### T2 — Durable agent / SDK / external-worker portability *(BLOCKED)*

Every external-worker PR is in the stale-`127ffaff` cohort and stacked on the never-reviewed `codex/external-worker-hardening-v1` (F2). `#416`/`#417` are the same commit. The only current-main members are `#451` (already placed in T5) and `#471` (checks in progress, 47 files).
- **Action:** this train must be **reconstructed**, not promoted. See §7.

### T3 — Adaptive Computer Use *(BLOCKED)*

`#453` is green and clean (promoted in T5). `#479` conflicts with `#453` on 6 files (`computer_agent.rs`, `computer_use/mod.rs`, `host.rs`, `lib.rs`, `desktop/src-tauri/src/commands.rs`, `desktop/src-tauri/src/computer_use.rs`). `#437` and `#438` are in the stale cohort. `#446`→`#448` are superseded/unqualified.
- **Action:** promote `#453` alone; rebase `#479` onto it and re-review.

### T4 — Packaged / VM Computer Use *(BLOCKED — claim class outruns evidence)*

`#445 → #449` conflicts immediately on 7 files including `computer_use/helper_authority.rs` and `package_identity.rs`. `#463`, `#449`, `#452`, `#450`, `#439` all carry packaged claims; the only hosted check any of them runs is `desktop`, which does not build, sign, or launch a package. **No packaged claim in this repository is currently qualified by a hosted gate.**

### Semantic Help — *no promotable head at all*

All four Semantic Help PRs (`#412`, `#413`, `#419`, `#422`) are in the stale-`127ffaff` cohort with 107–112 conflicts each, and `#412`/`#413` are superseded by `#419`. The train must be rebuilt from `#419`'s content on current main.

---

## 5. Next safe promotion candidates

These five are the recommended next promotion, **in this order**. They were simulated as a sequence onto `67e29bd` with `git merge-tree` and every step came back clean; the resulting tree was then materialised and compiled.

| # | PR | Head SHA | Files | Hosted check | Why it is safe |
|---|---|---|---|---|---|
| 1 | #344 | `122a792e7c81aefc3d81184fe29754644aabfb0f` | 6 | `desktop` green | 6 files, all under `evals/certification-lab/` plus one doc. No crate source touched. |
| 2 | #345 | `e1f92cbe14d8c0b4a2b2dd4cbbb87b71dd6e8cfa` | 1 | `desktop` green | 1 file: `tests/manager_supervisor.rs` (+382). Test-only addition; adds coverage, removes none. |
| 3 | #346 | `af0c307f0512ec6dec41ad6fe138b73d860721e1` | 1 | `desktop` green | 1 file: `src/orchestration/manager.rs` (+297). Strict directive-envelope parsing — narrows accepted input. |
| 4 | #347 | `68a6dc2bde9a4e05466f2520a8c26f59a0701dbc` | 1 | `desktop` green | 1 file: `tests/manager_mcp.rs` (+236). Test-only addition. |
| 5 | #348 | `41cba0cd680d73a9473a427c0be24f9b8fbf19cf` | 1 | `desktop` green | 1 file: `src/orchestration/worker.rs` (+185). Certifies the privilege-amplification guard. |

**Local gate evidence (run in this session, in an isolated worktree, tests unmodified):**

```
base        67e29bd34dc64049432c715c93c2cef2185c63ea
simulated   #344 -> #345 -> #346 -> #347 -> #348   (all CLEAN)
GATE_PLACEHOLDER
```

**Two caveats that must travel with this recommendation.**

1. `#346` and `#348` change production authority-adjacent code. A green `desktop` check is not semantic qualification — it is one job. Both need a human read against the manager/worker authority contract before promotion.

2. Promoting `#344` rewrites `evals/certification-lab/src/probes.rs`, which `#414` also rewrites. `#414` will need a rebase afterwards.

The wider 14-PR sequence `#344 #345 #346 #347 #348 #341 #342 #349 #433 #451 #453 #359 #469 #406` **also** simulated clean and compiled clean (91 files, +37,514/−1,063). It is recorded as evidence that the three ready trains do not collide with each other — not as a recommendation to promote fourteen drafts at once.

---

## 6. Branches that must NOT be merged

### 6a. Never-reviewed base branches (no PR has ever existed for these)

| Branch | Tip | Commits ahead of its fork point | Files vs main | Open PRs stacked on it |
|---|---|---|---|---|
| `codex/external-worker-hardening-v1` | `8ad3be07eb27` | 132 (from `127ffaff`) | 293 | 15 |
| `codex/cu-packaged-security-hardening-v1` | `404ea3c2c46b` | 422 (from `67e29bd`) | 256 | 1 (#439) |
| `codex/help-center-a11y-candidate-v1` | `7aaa7464e46c` | 20 (from `67e29bd`) | 17 | 1 (#380) |

None is an ancestor of `main`. None has ever been opened as a pull request — no `refs/pull/*/head` resolves to any of these tips. Merging any of them would land 132–422 commits of unreviewed work in one move.

### 6b. Stale-fork PR heads — 46 branches that cannot merge as they stand

Every one forks at `127ffaff`, is missing 72 `main` commits, and conflicts on 107–116 paths. Listed by PR:

| PR | Branch | Head | Conflicts |
|---|---|---|---|
| #381 | `cursor/external-worker-hardening-b019` | `faaf4074b1c7` | 111 |
| #382 | `codex/desktop-release-workflow-parser-fix-v1` | `aa70569b2c29` | 112 |
| #384 | `codex/external-worker-monitor-recovery-v2` | `72ea28175f29` | 115 |
| #385 | `cursor/bridge-nested-lockfile-sdk-0043` | `8b4f2ec57f4b` | 115 |
| #386 | `cursor/strict-clippy-repair-8f65` | `6b1ec1d33681` | 115 |
| #387 | `cursor/external-worker-list-archive-4bd3` | `c103594129c6` | 115 |
| #388 | `cursor/strict-clippy-warnings-a464` | `60ecc2f2d800` | 115 |
| #389 | `cursor/ledger-unavailable-public-taxonomy-b571` | `4631956c10e2` | 115 |
| #390 | `cursor/hosted-gate-public-errors-63c4` | `b5dae13f6f2a` | 115 |
| #391 | `cursor/sdk-root-charset-67bc` | `c7f28d81652e` | 115 |
| #394 | `cursor/enterprise-gateway-evidence-0921` | `a248596297d4` | 111 |
| #396 | `codex/pr394-release-workflow-context-v1` | `d1671b6b4a69` | 111 |
| #397 | `cursor/enterprise-gateway-correction-5745` | `2daf89ad8cce` | 111 |
| #398 | `cursor/enterprise-gateway-low-findings-3bd3` | `faa503633747` | 111 |
| #400 | `cursor/consumer-conformance-b64a` | `4f158d119f8f` | 115 |
| #402 | `cursor/consumer-conformance-fix-4aa5` | `f4a469759f42` | 115 |
| #403 | `cursor/sdk-boundary-projections-606e` | `ba802ad549c1` | 115 |
| #405 | `cursor/external-worker-production-path-ec26` | `697c28bdc2e4` | 112 |
| #407 | `codex/pr405-unsupported-error-v1` | `d564fbdff3d6` | 112 |
| #411 | `claude/grok-build-editor-readiness-v1-hcufhd` | `d7bde3ce39b7` | 113 |
| #412 | `claude/semantic-help-core-v1-y53hgm` | `342a3bacca02` | 107 |
| #413 | `claude/semantic-help-authority-v2-y53hgm` | `56146e4d893f` | 109 |
| #415 | `claude/durable-agent-p0-reconstruction-v1-25y7yd` | `6cb00faf8fa4` | 114 |
| #416 | `claude/external-worker-gate-4-zj3elc` | `33b74ce6a7f4` | 111 |
| #417 | `claude/external-worker-gate-5-prod-surface` | `33b74ce6a7f4` | 111 |
| #419 | `claude/semantic-help-authority-v3-y53hgm` | `d171216232b0` | 112 |
| #420 | `cursor/grokptah-ui-passive-run-5a57` | `6ce8eb331630` | 107 |
| #421 | `cursor/operator-consent-recovery-ux-772c` | `7fa04e353bae` | 111 |
| #422 | `claude/semantic-help-domain-v1-y53hgm` | `e12a3396516f` | 107 |
| #423 | `codex/desktop-agent-sdk-lock-repair-v1` | `712f41be6532` | 111 |
| #424 | `codex/pr423-continuity-repair-v1` | `6c1c4c3cd8d0` | 111 |
| #425 | `grok/self-host-continuity-v1` | `8827be56ac4f` | 111 |
| #426 | `grok/self-host-alpha-v1-pr424` | `58bff58a047b` | 114 |
| #427 | `grok/self-host-authority-v1` | `e616a2150ca2` | 116 |
| #428 | `claude/semantic-help-authority-v1` | `d57ad0be3744` | 111 |
| #429 | `claude/grokptah-durable-swarm-control-l8zy0m` | `437424c49df9` | 111 |
| #430 | `claude/computer-use-substrate-pr424-obejz2` | `e732b5da3207` | 111 |
| #432 | `claude/grokptah-packaged-qualification-vqk7rd` | `698e445e2fec` | 111 |
| #434 | `claude/release-evidence-qualification-v1-ax0kb0` | `849f67625462` | 107 |
| #436 | `claude/grokptah-computer-use-arch-783x6r` | `b0e9a75c494b` | 107 |
| #437 | `claude/adaptive-small-model-controller-06t58u` | `46ab6c3682b2` | 107 |
| #438 | `claude/grokptah-observation-grounding-2p4zlg` | `b0f41f462c22` | 110 |
| #440 | `claude/grokptah-computer-use-benchmark-dyuhi7` | `84c362e954ee` | 107 |
| #441 | `claude/sdk-service-adapter-qualification-8n5t72` | `29e005f46ae6` | 111 |
| #442 | `claude/grokptah-agent-hardening-348i2w` | `e997c6436cb0` | 110 |
| #454 | `claude/cloud-opus-self-host-4bxur7` | `1f99ffe735f4` | 116 |

**46 branches.** `#416` and `#417` share the head `33b74ce6a7f4` — two PRs, one commit.

---

## 7. Retirement proposal — requires human approval

**Nothing here has been actioned.** No PR was closed, retargeted, undrafted, or marked. This is a proposal only.

### 7a. Closable on ancestry evidence alone (52 PRs)

Each is strictly contained in a newer open PR — its head is a git ancestor of the successor's head, so closing it loses no content. Full list in §3 and in `pr-inventory.json` under `superseded_by_open_prs`.

**Before closing any of them, confirm the successor is itself viable.** Most successors are in the stale-`127ffaff` cohort and cannot merge either; closing the ancestor then leaves *both* the content and its only reviewed history behind a branch that needs rebuilding. Safe subset to close today: the ones whose successor is in the current-main cohort. Everything else should wait for §7b.

### 7b. Reconstruct-then-retire (46 PRs, the stale cohort)

These cannot be rebased mechanically — `codex/external-worker-hardening-v1` sits between them and `main`, and it has never been reviewed. Proposed sequence:

1. Open a PR for `codex/external-worker-hardening-v1` itself so its 132 commits get a review surface, **or** declare it abandoned.
2. Rebuild the four coherent lines on current main as fresh heads: external-worker (`#416`/`#417` content), Semantic Help (`#419` content), self-host authority (`#427`/`#454` content), isolated visual Computer Use (`#430`/`#432` content).
3. Keep the 46 originals **open** until each replacement head is green. They are the only record of the reviewed intent.
4. Retire the originals only after the replacements merge.

### 7c. Duplicate

`#416` and `#417` are the same commit and the same tree. One should be closed in favour of the other — the human picks which title survives. Both are in the stale cohort, so this is a §7b item, not a today item.

---

## 8. Residual uncertainty

Stated plainly, because several of these bound how far the plan above can be trusted.

1. **Hosted checks were observed for 33 of 129 PRs**, not all. I pulled check runs for the promotion pool and the prioritised set; the remaining 96 show `none-observed`, which means *I did not query them*, not that they have no checks. GitHub's `status:` search qualifier returns nothing for this repo because CI reports check-runs rather than commit statuses, so bulk bucketing was unavailable.

2. **The local gate is Linux; the hosted gate is macOS.** `desktop/src-tauri` was not compiled here — its GTK/webkit and macOS system dependencies are unavailable in this container. `libdbus-1-dev` had to be installed before even the bridge crate would configure. A green local `cargo check` therefore says nothing about the desktop crate or about anything macOS-specific.

3. **No packaged, live-provider, or isolated-VM claim was verified.** 82 of 129 PRs carry packaging markers and 11 carry live-gated markers. The only check most of them run is `desktop`. I classified claim *class* from diff content; I did not qualify any claim.

4. **Domain classification is a path/title heuristic**, not a semantic reading. `computer-use` dominates (79) largely because the stale cohort shares one base branch that touches those paths. Treat the domain field as a routing hint.

5. **Supersession is containment, not equivalence.** A PR being an ancestor of another proves the content is present; it does not prove the successor's *additional* changes are wanted. Every close in §7a still needs a human to agree the successor is the intended direction.

6. **`main` may have moved.** Everything here is anchored to `67e29bd`, which matched `origin/main` when the analysis ran. If `main` advances, re-run the merge simulation before promoting — the clean results are anchor-specific.

7. **The #489-vs-#488 choice is not mine to make.** I can show they conflict on 7 files and that #489 matches issue #477's written donor plan more literally. Which spine is architecturally right is a human decision, and both are currently red.

8. **`#471` and `#468` were in progress** when their checks were read; their rollup may have changed since.
