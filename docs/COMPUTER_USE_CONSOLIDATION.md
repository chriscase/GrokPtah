# Computer Use consolidation train (current main)

Base of record: `origin/main` `67e29bd34dc64049432c715c93c2cef2185c63ea`.

Every head below was fetched and its ancestry recomputed locally on
2026-08-29. **PR bodies are not trusted as identity**: two of them state a
head that is no longer current. Ancestry is stated as distance from the base
commit above.

## 1. Exact donor inventory

| PR | Branch | Recorded head (actual) | Body-claimed head | Base | Ahead / behind main |
|---|---|---|---|---|---|
| #446 | `grok/cu-adaptive-eval-harness-v1` | `2274cc69d7f24e085f7da858f0ec7249f744262e` | (unstated) | `main` | 1 / 0 |
| #448 | `grok/cu-adaptive-evaluator-authority-v2` | `906ef985eb5bfadfa23daebd66f1f57162d16b35` | `906ef985…` ✅ | **#446 head** `2274cc69…` | 6 / 0 |
| #449 | `grok/cu-packaged-vm-authority-integration-v1` | `8d0478f75a032f16c6ad741a8463479879899c86` | `61ad540b…` ❌ **stale** | `main` | 5 / 0 |
| #453 | `claude/computer-use-boundary-adapter-4jg5og` | `ca78af4f7c4b1678b160d7faf898d640df69bd07` | `ca78af4f…` ✅ | `main` | 1 / 0 |
| #463 | `claude/grokptah-computer-use-authority-otohf1` | `f177ae82b3a133ba1840aa1e79b0a26d6d496c8e` | (unstated) | `main` | 5 / 0 |
| #472 | `claude/cu-adaptive-runtime-integration-v1-3zh7xo` | `a97a8ab221f6d7c24e26042f8983658e577a73df` | `a44dbb85…` ❌ **stale** | **#473 head** `df21aa72…` | 4 / 0 |
| #473 | `claude/cu-sealed-current-frame-boundary-oqoq9c` | `df21aa72fc45a0714acab2001f13d78adc5e7d10` | — | `main` | 1 / 0 |
| #488 | `grok/canonical-authority-spine-g1g4-v1` (**G1–G4**) | `adbfc79702fc1a7e59ae2563dc92124ebe7a0064` | — | `main` | 1 / 0 |

Every donor is **0 behind** `67e29bd`, so all are rooted at exact current
main; none needs rebasing to be read as a current-main donor. Only #448 and
#472 are *stacked* (on #446 and #473 respectively), so neither is a
current-main donor on its own — their content must be re-derived, not
cherry-picked.

### Local-only witness `807143eb`

**Unavailable — not reconstructed, not inferred.** `git cat-file -t 807143eb`
in this Cloud clone returns `Not a valid object name`, and no dangling object
supplies it. The goal permits using it *only if the object is available*, so
it is recorded as absent and contributes nothing to any disposition below.
Any claim about its contents would be fabricated.

## 2. KEEP / REWRITE / REJECT

Scope of this train: the model→proposal boundary and the adaptive vocabulary
**above** the safety kernel. `ComputerPolicy` stays the only physical-action
authority; nothing here dispatches.

| # | Donor content | Disposition | Reason |
|---|---|---|---|
| #446 | `evals/computer-use-adaptive` synthetic harness (33 files, +6215) | **KEEP WHOLE — out of this train** | A standalone eval crate that touches no production runtime. It should promote on its own eval lane; duplicating it here would add 6k lines that this slice does not need. |
| #446 | Canonical naming decision: `economy` / `balanced` / `high_assurance`, aliases `efficient`→economy, `frontier`→high_assurance, ingest-only | **KEEP (semantics)** | This is the naming authority named by issue #435. Re-implemented here as the production vocabulary; the eval crate keeps its own copy. |
| #448 | Independent recomputation of `release_failing`; `Result`-returning digest paths | **KEEP — out of this train** | Correct, but it is evaluator-internal and stacked on #446. Not production runtime. |
| #449 | `IsolatedPreflight` / `HelperSupervisor` env-pinned expectations | **REJECT as donor code** | Superseded by #463, which replaced the same five P0s rather than patching them. Its own body is stale by 5 commits. |
| #449 | The *nonclaim* discipline (no notarized helper, no TCC, no VF boot, disk gate) | **REWRITE (semantics only)** | Adopted as a typed, fail-closed verdict rather than prose in a PR body. |
| #453 | `computer_agent/boundary.rs` (2695 lines) | **REWRITE — fixtures only** | Per the goal, #453 contributes *fixtures only*. Its boundary rewrites `computer_agent.rs` wholesale (+526/−233) and edits desktop and docs; that is far wider than this slice. The parsing *properties* it asserts are re-derived against current main. |
| #453 | Profile names `Efficient`/`Balanced`/`Frontier` | **REJECT** | Contradicts canonical #435 vocabulary. Admitted only as ingest aliases, never as output. |
| #463 | `CodeIdentityProbe`, `PackagedTrustRoot`, `CleanupReceipt::observe`, one-authority gate | **KEEP — out of this train** | Sound and large (59 files, +12446), but it is the *packaged* lane. This train must not duplicate it. |
| #463 | "`pass` is unreachable from any code in this PR"; verdict `unavailable` with named reasons | **REWRITE (semantics)** | Adopted as `PackagedQualification`, which cannot construct a non-`Unavailable` verdict from a simulator. |
| #463 | `sha256_hex(&hasher.finalize())` double-hash defect | **KEEP as a known hazard** | Not present in this slice's code; recorded so the same shape is not reintroduced. |
| #472 | Profile vocabulary + monotone budgets + honest-unknown usage | **REWRITE onto main** | The semantics are right, but the head is stacked on #473 and cannot be taken as a current-main donor. Re-derived here without #473's `seal`/`receipt` modules. |
| #472 | `AdaptiveRecord` on `ComputerRun`, `#[serde(default)]` legacy → fail-closed | **REWRITE onto main** | Same reason. Adopted as a durable, run-keyed, `Option`-typed record that reads as *no authority* when absent. |
| #472 | Removal of `low_confidence` / `contradictory_semantics` (signals with no producer) | **KEEP (semantics)** | Publishing a signal nothing raises is a false claim. Not reintroduced. |
| #472 | `begin_turn` capability-generation digest | **REJECT here** | It is #472's own stand-in for the canonical #458/G1–G4 generation. Re-implementing it would duplicate authority the G-train owns. This slice binds to `control_epoch`, which already exists on main. |
| #473 | `seal::accept_model_proposal`, `receipt.rs`, sealed-boundary tests | **REJECT as donor code — semantics KEPT** | #473 is a separate open PR. This slice does not vendor it; it implements the narrower "host mints, model echoes" property directly on main so the two do not collide. |

## 3. Dependency graph (this train vs G1–G4)

```
        main 67e29bd
             │
   ┌─────────┼──────────────────────────────┐
   │         │                              │
 G1–G4     THIS TRAIN                  other lanes
 (#488)    (proposal boundary +        (#446/#448 evals,
   │        adaptive vocabulary)        #463 packaged)
   │            │
   │            │  binds to, never redefines:
   │            ├── run_id / owner_session_id      (main)
   │            ├── control_epoch                  (main)
   │            └── observation_id + sequence      (main)
   │
   └── owns: host principal, IssuedAuth/AuthContext,
             process generation, durable single-writer
             persistence, resource creation binding
```

**Non-duplication rule.** G1–G4 owns *who* the host is and *how* durable
records are authenticated. This train owns *what a model may say* and *which
profile vocabulary is canonical*. The proposal nonce minted here is a
per-observation anti-fabrication challenge, **not** a principal, credential,
or auth generation — when the G-train lands, principal binding is added
alongside it rather than replacing it. This train adds no store, no ledger,
no second authority, and no second agent runtime.

## 4. Backlog reconciliation

| Issue | What this train does | What it does **not** close |
|---|---|---|
| **#272** provider/model conformance | Unknown/underqualified route ⇒ no proposal admitted; declared capability is observation-only. | Live gateway probes, measured capability tiers, comparison report. **Stays open.** |
| **#274** adversarial release gate | Adds fabricated-DTO, stale-observation, unknown-field, lease-loss, stationarity, crash/restart, and simulator-confusion tests. | Native/packaged fixtures, retention/quota proofs, hardware matrix. **Stays open.** |
| **#288** isolated visual backend | Only the fail-closed nonclaim: a VM/isolated verdict cannot read `pass` without host-observed evidence. | The backend itself — owned by #447/#463. **Stays open.** |
| **#363** multi-agent surface leases | `control_epoch` loss invalidates an outstanding proposal (a lease-loss fence at this layer). | Durable `ComputerSurfaceLease`, conflict domains, queueing. **Stays open.** |
| **#435** adaptive profiles | Canonical Economy/Balanced/High Assurance vocabulary with ingest-only aliases and monotone, safety-invariant budgets. | Live campaign, packaged qualification, cockpit copy, escalation UX. **Stays open.** |
| **#444** packaged macOS identity | Nonclaim discipline only. | Signing, notarization, TCC, hardware acceptance. **Stays open.** |
| **#492** release-train consolidation | Supplies this train's exact inventory, dispositions, and supersession guidance. | Bulk closure, branch deletion, retirement — explicitly out of scope. **Stays open.** |

## 5. Supersession guidance per donor

No branch is deleted, no PR is closed, and no PR is merged or undrafted by
this train. Recommended dispositions for a later human-approved retirement
list:

- **#446** — keep open on its own eval lane. Not superseded. Its naming
  decision is now also enforced in production code; if the two ever diverge,
  #446's report identifiers and this train's `AdaptiveProfile` must be
  reconciled in one change.
- **#448** — keep open, stacked on #446. Retire together with #446 or not at
  all; retiring #446 alone would strand it.
- **#449** — **superseded by #463** for the packaged-helper authority. Its
  body head is 5 commits stale, so any review of it must re-read
  `8d0478f7`, not `61ad540b`. Recommend closing only after #463 promotes.
- **#453** — **partially superseded by this train** for parsing semantics.
  Its `Efficient/Balanced/Frontier` naming is rejected outright. If #453 is
  promoted later it must adopt the canonical vocabulary first; otherwise the
  repository would carry two spellings.
- **#463** — **not superseded.** It is the packaged lane and should promote
  independently. This train deliberately adds none of its files.
- **#472** — **superseded onto main by this train** for the profile
  vocabulary, durability shape, and honest-usage accounting. It remains the
  reference for the adaptive controller and cockpit surface, neither of which
  this train implements. It cannot promote before #473; this train can.
- **#473** — **not superseded.** Independent. If #473 promotes first, this
  train's parser should be re-pointed at `seal::accept_model_proposal` as the
  single universal validator rather than keeping two entry points.
