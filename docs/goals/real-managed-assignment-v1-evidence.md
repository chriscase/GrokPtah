# Real Managed Assignment v1 — implementation evidence

Status: **PARTIAL — FOLLOW-UP REQUIRED**. Safe production machinery is implemented. One live provider-backed child attempt made one upstream request and failed closed without a verified candidate. No retry or source application occurred. The goal is not complete.

Frozen goal: `docs/goals/real-managed-assignment-v1.md`, commit `81c8d08f14a61088e9a9adec6c6fef28b0611215`.

Branch: `grok/real-managed-assignment-v1`; draft PR [#580](https://github.com/chriscase/GrokPtah/pull/580).

Base: `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d`; tree `f524b705a3b88cb895309506bf726e76c91874bf`.


## Review identity

| Field | Identity |
| --- | --- |
| Repository | `chriscase/GrokPtah` |
| Branch | `grok/real-managed-assignment-v1` |
| Worktree | `/private/tmp/GrokPtah-real-managed-assignment-v1` |
| Integrated base SHA | `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d` |
| Integrated base tree | `f524b705a3b88cb895309506bf726e76c91874bf` |
| Frozen goal commit | `81c8d08f14a61088e9a9adec6c6fef28b0611215` |
| Frozen goal tree | `315bf2210bc1a47ca889bbeb4812b794acf60902` |
| Final functional SHA | `97bf4d5a639650d68e45eaf68f52d66c17b22688` |
| Final functional tree | `f42634387bd2fe7073feee002d3f9353dce41559` |
| Exact-functional Desktop | [36305039955](https://github.com/chriscase/GrokPtah/actions/runs/36305039955), attempt 1 |
| Draft PR | [#580](https://github.com/chriscase/GrokPtah/pull/580) |
| Final published SHA/tree and independently read remote SHA | Recorded in the PR description and final handoff after the evidence-only commit is pushed and read back from GitHub/Git. A committed file cannot contain its own enclosing commit SHA/tree without changing that identity. |

The publication commit changes only this document and sanitized JSON artifacts. Final remote verification must compare both Git remote branch SHA and GitHub's independently returned commit/tree, prove the functional revision is an ancestor, and verify that the functional-to-publication diff contains only these evidence paths. The working tree is then checked clean. Neither this PR nor any follow-up goal is authorized for merge/release/deployment here.

## Predecessor integration

PR [#579](https://github.com/chriscase/GrokPtah/pull/579) was independently refreshed before merging. Its head was exactly `8451fb098a4b04d47110752fc37ea7b1e4ecb227`, tree `f524b705a3b88cb895309506bf726e76c91874bf`. The reviewed functional revision was `56757475fdfeba18247e936e16f240ecfa9f7f8f`, tree `341591e61ab0f51bac0d4fc89e364fc4460088d5`. The sole subsequent commit and sole changed file were the known evidence-only commit/document. Ancestry was intact; GitHub reported MERGEABLE/CLEAN and its desktop check succeeded. Reviewed Desktop run [36258025951](https://github.com/chriscase/GrokPtah/actions/runs/36258025951), attempt 1, pull_request, exact reviewed functional SHA, was completed/success with no failed steps. The published-tip Desktop check also succeeded, run 36260199676.

The explicitly authorized integration used a merge commit, `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d`, with parents `0dbe51c8aa94e3f26543be7a90e0085029487de7` and `8451fb098a4b04d47110752fc37ea7b1e4ecb227`. Both reviewed and published SHAs are ancestors of refreshed main. Its tree equals the reviewed published tree. Verified Change source and prepare/start/apply/discard operator tools are present. No historical branch was deleted or rewritten. Post-merge smoke passed formatting and both resealed-admission regressions with a fresh disposable store.

The earlier preflight made no implementation changes, created no implementation branch, and dispatched no provider request. It stopped solely because the integrated predecessor was then unavailable. Preflight provider dispatch count: **0**.

## Integrated production path before implementation

Paths below are under `crates/codegen/grokptah-agent-bridge` unless stated otherwise.

| Boundary | Integrated path and behavior |
| --- | --- |
| Operator entry | `src/mcp_control.rs`: `ptah_prepare_verified_change`, `ptah_start_verified_change`, `ptah_get_work`, approval/apply/discard tools; desktop `VerifiedChangePanel` uses these controls. |
| Work creation/assignment | `src/orchestration/service.rs::start_verified_change`: existing manager plan/advance creates and assigns Work; no second ledger. |
| Execution approval | Existing revision-bound Work authorization and `ManagedGrokClaimFence` in service/store. |
| Executor selection | Agent managed-execution policy selects `GrokBuildIsolatedReview`; `admit_one_managed_grok_work` validates scope and profile. |
| Grok Build launch | `src/grok_build.rs::launch_grok_build`; one supervised process, no process retry. |
| Credential acquisition | Host `src/auth_store.rs`; adapter requires `CredentialLeaseResolver`. Operator environment setup on integrated main explicitly refuses file-only leases. Existing `HostLeaseAuthority` models test-provider acceptance and is not production xAI containment. |
| Process/environment isolation | Adapter clears inherited environment, uses a private home and bounded PATH, process-group supervision, finite time/output, disabled compatibility/plugin/hook surfaces. Integrated mutation sandbox restricts writes but still broadly permits reads/network; that is insufficient for credential containment. |
| Repository isolation | Private clone with separate Git directory, no remotes, exact base/ref/control fingerprint, allowlisted mutations; required-check Work retains the candidate instead of applying it. |
| Completion evidence | `finalize_managed_grok_task` validates adapter result, advisory and mutation proof; missing/uncertain completion enters review rather than success. |
| Candidate retention | Adapter retention plus store `candidate_snapshot_dir`; host binding verifies the retained snapshot. |
| Independent checks | `src/verified_change.rs`: registered external oracle, sealed check authority, bounded argv/cwd/env and macOS check confinement; service `bind_required_candidate_verification`. |
| Review/approval | Work projections, verification record, explicit content/bundle/profile-bound approval. |
| Source application | Existing `apply_verified_change`, approved bundle and crash-safe admission/source-intent recovery. |
| Restart recovery | Store admission/source-intent/finalization recovery; service `recover_or_heartbeat_managed_grok` marks an absent supervised task uncertain and does not redispatch. |
| Cancellation | Work cancel plus supervised task cancellation and adapter process-tree termination; unproved termination fails closed. |
| Late result | Service validates current intent/Work attempt and terminal Work state before finalization; cancelled Work must not become apply-authorizing success. |

One concrete identity gap to address is that the integrated Grok admission has an Attempt and managed intent but leaves `run_id` empty. This goal requires an observable exact Work/Run/Attempt relationship through the existing Run store, not another execution ledger.

## Credential investigation

Installed Grok Build is `1.0.41 (4220f3b224a6) [stable]`. Version/help discovery was preserved from preflight; behavior and isolation still require actual installed-binary probes.

The host currently has a Grok auth file. Relevant API/management-key environment variables are absent; matching host keychain records were not found or available. No secret was printed during this presence-only check. Presence does not prove that the login is usable.

[xAI management authorization](https://docs.x.ai/developers/rest-api-reference/management/auth) supports model/endpoint ACLs, expiration and key disable/delete, but requires a separate management authority that is not configured here. Its rate limits do not abort requests already in flight. This is a supported alternative, not an upstream-impossibility claim.

[Grok's custom-model documentation](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/11-custom-models.md) documents custom endpoints and bearer authentication. The concrete implementation direction is a host-owned, per-assignment relay capability: real credentials remain in existing GrokPtah custody, a local worker capability is accepted only by the relay, and revocation/expiry stops future forwards. Existing `provider_transport::send_provider_request` remains the sole credential-bearing upstream wire boundary and preserves uncertain outcomes. Already-sent requests cannot be undone; that uncertainty must remain explicit.

The production relay and strict confinement passed concrete native and installed-binary probes before the live gate opened. The live request did not produce an accepted completion or a candidate. Test-provider probes establish protocol/isolation behavior and are explicitly separate from live qualification.

No donor code was imported. The frozen criteria have not changed.


## Final production path

The authenticated existing MCP entry is `ptah_prepare_verified_change` → `ptah_start_verified_change` → `ptah_verified_change_status`. The desktop panel already uses these controls. Service startup and desktop MCP startup now await `configure_managed_grok_from_operator_env`; the old file-backed production lease configuration is refused.

`ManagedProviderRelay` (`src/managed_provider.rs`) resolves the existing production `auth_store` credential in the host. A UUID-bound, opaque per-intent capability is minted before claiming; the actual local bearer is supplied only after repository isolation. Work, Run and Attempt use existing stores and journals. The host creates an existing Run, activates the persistent Agent, links the exact Attempt and persists Dispatching before launching a supervised child. Claiming recovery adopts only the exact scoped managed Run. Missing supervision after restart enters review, never another paid dispatch. Existing finalization recovery closes the Run and releases the Agent.

`src/grok_build.rs` clears the child's environment and pins actual Grok 1.0.41 behavior. The child receives only a relay bearer in `GROKPTAH_MANAGED_CAPABILITY` and `XAI_API_KEY`; its auth file is `{}`, not the operator's auth. Private configuration disables auto-update, telemetry, workflows, title refresh, turn summaries, subagents, web/MCP/plugins/hooks and compatibility imports. Installed documentation uses singular `[model.NAME]`. The child is limited to file tools; pinned custom model selection is `grok-build-0.1`. Actual stdout uses seven fields when usage is available; production validation requires those usage/model/turn fields, the durable CLI turn/usage journals, and independently settled host sends. The legacy ten-field parser remains intact. Missing usage is not converted to zero cost.

The worker's deny-default macOS profile grants data access only to its private home/checkout and the exact CLI plus narrow public runtime paths. Outbound network is restricted to the assignment relay port. No operator data, source checkout, external oracle, keychain, publishing credentials or other local service is granted. Public `/System/Library` and Cryptexes are allowed; broad `/System` is deliberately absent because `/System/Volumes/Data` aliases operator data. The native canary probe proves denied operator reads, denied oracle writes and denied another port, while the assigned relay and candidate writes work.

Repository cloning, allowlist validation, exact source fingerprint, retained promotion record, candidate verification and explicit apply/discard stay on the integrated Verified Change path. Host check authority revision 2 executes candidate code with data access only to candidate/oracle/output and public system toolchains; only output is writable and network is denied. Oracle definitions are external to the worker's writable scope. Revision 1 remains available for existing workflows; production relay assignments require sealed revision 2 with no network.

Readiness runs the actual CLI version/inspect inside confinement without credential resolution or inference. Its temporary probe is outside the configured empty worker root, avoiding a status/admission race. Source Git identity, clean tree, path ceilings, required tooling, registered oracle and protected writable isolate are assessed before admission; the existing audit records the result. READY proves local prerequisites and credential freshness, not upstream acceptance, entitlement, billing or a promised successful task.

## Credential bounds and revocation

The child capability is not an xAI API key or operator OIDC token. It is useful only at the host relay. The relay restricts the upstream route to the production xAI API or Grok CLI chat proxy, the model to `grok-build-0.1`, and accepted tools to file/meta-file-tool calls. Unknown routes, models, tools, duplicate bodies, redirects and concurrent requests cannot become fresh sends. Every upstream send passes through the existing canonical `provider_transport` authority, with an opaque permit observation durably admitted before dispatch. The relay is not a second provider-send ledger; its counters are attached to the existing managed intent and Run.

Maximums: 180 seconds from capability injection, six physical request admissions, 1,024 output tokens per request, twelve tool calls, 16,000 aggregate tokens, 64 KiB request body and 256 KiB response. Before each send it reserves a conservative raw-byte input allowance plus output ceiling against settled totals. Response overruns invalidate the lease and remain uncertain. Each accepted response requires exactly one consistent input/output/total usage receipt; missing or duplicate usage invalidates the capability before the child receives the response. Tighter Work/Agent/server ceilings are bound to the individual capability before injection and cannot be expanded after issue. Work/Run admission and projections intersect these ceilings with profile, server and Agent bounds. The qualification server also limits the prompt to 16,000 bytes. No dollar cost is invented when provider usage is unavailable.

Cancellation, expiry, shutdown and normal completion revoke the local capability. Child disconnect cannot erase an admitted send: the canonical observer runs before wire handoff; a settlement guard records dropped futures and the adapter waits for provider quiescence before finalization. Revocation prevents **future** forwards from this worker. It does not revoke the host's original credential and cannot undo a request already accepted remotely. In-flight or incomplete usage stays uncertain; restart neither revives the old relay bearer nor redispatches an absent process.

Primary upstream references: [custom models](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/11-custom-models.md), [management authorization](https://docs.x.ai/developers/rest-api-reference/management/auth), [management guide](https://docs.x.ai/developers/management-api-guide), [coding model](https://docs.x.ai/developers/models/grok-build-0.1). Upstream expiring/model-scoped keys remain an alternative requiring a management authority unavailable on this host. That absence is not an impossibility claim or the reason for this partial result.

Installed binary: `/Users/chriscase/.grok/downloads/grok-1.0.41-macos-aarch64`; version `1.0.41 (4220f3b224a6) [stable]`; digest `sha256:9c844eb13365180787d9ad22b2b3748a024be8e1ed845253cc114781b31c591d`. Installed configuration/headless/custom-model documentation was inspected. Three native actual-CLI protocol cases used the same production adapter, Git/filesystem and canonical transport with a local upstream fixture: successful accounted completion, HTTP 503 refusal with no retry, and cancellation after the fixture observed a physical request. Each made exactly one fixture request, revoked the bearer and left source unchanged. These are **not live xAI qualification**.

## Actual live journey and failed admissions

Preflight provider dispatches remain **0**. This goal made **1 live upstream request**, **1 provider-backed worker attempt**, **0 retries**, and **0 applications**. Earlier unsuccessful admissions are preserved rather than reset or hidden:

| Qualification | Result | Work/Attempt | Live sends |
| --- | --- | --- | --- |
| `/private/tmp/rma-live-qualification-v1` | Control-token setup was corrected; manager then refused an over-broad Work budget before creating a Work. Fixed by intersecting actual assignment bounds. | None | 0 |
| `/private/tmp/rma-live-qualification-v1-bounded` | One durable attempt failed `isolation_failed` before child/capability injection because concurrent status created probe files in the empty launch root. Fixed by placing probes outside that root. | Work `d3a4c10a-6bb5-4e7d-9a3a-3c92636534a0`; Run `418174ae-e302-4a29-9734-7e5c1fc23e69`; Attempt `cd31e4d0-ad75-4a90-b872-bdc0bb9f0482` | 0 |
| `/private/tmp/rma-provider-qualification-v1-final` | Readiness ready; real child and upstream send; no trustworthy completion/candidate. Review then explicit discard. | Identities below | 1 |

The two durable qualification Attempts are distinct: the first never launched a child or sent a request; the second is the sole provider-backed child attempt. Neither Work was rearmed and automatic retry remains disabled.

The committed purpose-built fixture repairs canonical UTF-8 byte-length framing across `src/framing.py` and `src/decoder.py`. Four external oracle methods cover empty/ASCII/Unicode/embedded-colon data, canonical headers, exact lengths and malformed UTF-8. The host ran a confined **red baseline** before admission: exit 1, confinement revision 2, spec digest `sha256:662a864af79d45ace1bab977e1280c772aad280bb0b4848bcd1f2d82bff0976c`. No worker can modify this oracle.

- Operator entry: authenticated production loopback MCP, service execution surface.
- Repository: `qualification:utf8-frame-v1`; disposable source has no Git remotes.
- Base: `1f0120725be891b4ba165aca226e9cd34b27360d`; tree `1ef9c0872b4eadfaae5f75bbf2933c062cce67ec`.
- Session: `8478d315-239c-41d5-ab67-5c265a30d2d7`.
- Work: `c724d7eb-d321-4159-af79-7861ad0bb10b`.
- Run: `681606e4-b8a6-469f-ab80-ab246b861af7`.
- Attempt: `752e00ee-5b62-4038-bbbc-3fd40a016878`, number 1.
- Executor: `grok_build_isolated_review`; model `grok-build-0.1`.
- Physical authority attempt: `att_2ccf6034ee8b0389`; one durable wire admission and one settled HTTP response observation.
- Candidate: none; `sha256:missing` is an invalid sentinel, **not a candidate identity**.
- Verification: candidate check invalidated, not passed; no candidate code was claimed green.
- Completion: `closed-run:managed-worker-did-not-exit-cleanly`; no persistable advisory.
- Provider evidence: reserved 1, wire admissions 1, accepted completion responses 0, usage unavailable, accounting incomplete, uncertain true, revoked true. Numeric token zero placeholders are not proof of zero usage/cost. Exact numeric HTTP status/body were not retained; no unsupported claim is made about the provider's precise refusal reason.
- Disposition: explicit `ptah_discard_verified_change`, Work Cancelled; Run remains Interrupted, Attempt retained. No approval or source application.
- Reopen: review before disposition and two reopens after discard preserve exact identities, Attempt count 1 and wire count 1. Canonical audit still has one wire admission. Source diff remains empty and both source-file digests match the initial source.
- Secret retention: adapter scans actual stdout/stderr/home/checkout for its local capability before classification and destroys the private home/checkout. A separate parent-only scan of retained qualification files plus decompressed loose Git objects found 0 matches for existing operator credential bytes; no credential bytes were printed or retained. Scope/counts are in the sanitized scan artifact. Worker root was empty after termination.

Sanitized machine-readable records are committed under `real-managed-assignment-v1-artifacts/`: readiness, red baseline, live execution, review, discard, repeated reopen, canonical physical-send audit, zero-wire isolation failure, secret-retention scan, predecessor integration and completed hosted validation. `artifact-digests.json` records their exact content identities. Actual host stores and logs remain local under the disposable roots and `/private/tmp/rma-*.log`; credentials, custody keys and raw auth files are not published.

The live attempt was performed on the working production implementation before the final truthful projection clamp, transport heap-allocation adjustment, tighter per-assignment relay binding, mandatory per-response usage guard and canonical public-Xcode path correction. It was not repeated on the final functional commit. The actual Grok binary identity above is exact. Hosted CI below binds to the final functional revision; it does not retroactively qualify this failed live attempt.

## Frozen G1–G10 result

| Criterion | Result | Evidence and limit |
| --- | --- | --- |
| G1 | PASS | Existing authenticated entry, exact source/approval/scope/limits; one observable Work/Run/Attempt in real stores. |
| G2 | PASS | Actual confined CLI/version/inspect, source Git/oracle/isolate/credential freshness checks; durable zero-send denial. READY does not promise upstream acceptance. |
| G3 | PASS for bounded containment | Production host credential resolver and revocable local relay, actual child confinement and one real upstream send. No claim that the original xAI credential itself was revoked or a valid completion obtained. |
| G4 | FAIL | Deterministic two-file task was admitted, but the live worker produced no inspectable candidate or meaningful fix. |
| G5 | FAIL for live journey | Candidate identity/host verification pass real Git/store regressions; no live candidate exists to bind or verify. |
| G6 | PASS for review/discard | Actual failure enters durable review and explicit discard; no source mutation. Approve/apply of a live candidate was not exercised. Existing real Git/apply recovery regressions pass. |
| G7 | PASS for local durable boundaries | Nine restart/cancel boundaries below use real host/store/Git/FS components; installed CLI cancellation uses a cost-isolated upstream. Live failed review/discard survives repeated reopen. No paid live cancellation/application scenario is claimed. |
| G8 | FAIL | One bounded real request occurred safely, but no verified candidate/usage-complete outcome. No further paid attempt. |
| G9 | PASS | Production component regressions, strict native confinement, exact identity/reopen, source/check isolation and authority-preserving cancellation/application tests. |
| G10 | PASS on publication | Frozen goal, exact code/CI identities, sanitized evidence, draft PR and remote verification; this partial result is handed off without merging. |

## Restart and cancellation matrix

| Boundary | Concrete evidence |
| --- | --- |
| Restart before provider dispatch | `restart_at_admission_approval_and_apply_names_a_safe_action`; real stores/approval fences, no duplicate child admission. |
| Restart after durable dispatch before result | Same test and `cancel_and_restart_do_not_dispatch_or_apply`; absent task becomes uncertainty/review, no replacement dispatch. |
| Retained candidate before verification/review | `restart_after_candidate_persistence_and_verification`; real retained tree, check identity and durable store. |
| Verified candidate before disposition | Same test; exact digest/checks survive reopen; source remains unchanged until explicit apply. |
| Cancellation before dispatch | `cancel_and_restart_do_not_dispatch_or_apply`; no child/send. |
| Cancellation with child | Same integrated test, and actual installed-CLI cancellation after the local upstream received one request; process is stopped/capability revoked, completion uncertain. |
| Late child result after cancellation | Integrated cancellation test; cancellation stays authoritative and cannot grant apply. |
| Repeated reopen after successful apply | `repeated_store_reopen_is_idempotent_for_receipt_work_and_source`, legacy recovery regressions and explicit apply workflow; no second source effect. |
| Repeated reopen after discard | Integrated operator disposition test plus the real failed qualification's two post-discard reopens; no approval, new attempt or new upstream send. |

External provider responses are mocked only to isolate cost in this matrix. The adapter, process supervision, filesystem, Git source/candidate, store, check authority and apply/recovery boundaries are actual production components.

## Validation and failures

Hosted history is preserved rather than collapsed into a passing claim. The initial implementation `cc94c603829ae54fbeb3a7050498b0c70c7641ee` (tree `eff5faa04b016effc33f839eb0540ec726e36e35`) was followed by strict per-response usage validation at `ff415ebab918c107c82fa1354f7d577b72a8980a` (tree `31eecc8ce172877f56747f0b33753d5e37d92486`). Desktop run `36302732773` on the initial implementation was superseded/cancelled and is not qualification. Run `36302996863`, attempt 1, on the usage-validation revision failed the new private-check unit test: 703 passed, 1 failed, 1 ignored. The child Python oracle aborted with exit code 134 and a truncated diagnostic; the exact loader cause was not then retained.

`6ef3e5d7d52935a69fcd47b031ed74027315134d` (tree `08edc6667eea50121b1e381ad48346cb3d224d0e`) added narrowly validated canonical root-owned public Xcode selection, preserving all operator-data/write restrictions. Run `36303628757`, attempt 1, failed the same test with the same counts; this correction alone did not resolve the hosted failure. `9af032760b0bad53707df715869c2a8de54d8f80` (tree `66a990c147c8ef394fc98ab82905e062e7388f43`) added a bounded diagnostic only in the synthetic unit-test fixture. Its Desktop run `36304334776`, attempt 1, failed with the same test counts. The bounded diagnostic selected `/__grokptah_missing_public_xcode__` and showed the Python framework loader denied inside `/Applications/Xcode_26.6.app`. No production raw logging was added.

`ecb9a731aa66ba280cb3c9e0fbcc69468a8414f5` (tree `a928cf5f84912ba7ec12a59441c3e9fe6e85e658`) established root ownership and removed group/world write permission only on the selected public Xcode bundle in the disposable hosted runner. `97bf4d5a639650d68e45eaf68f52d66c17b22688` (tree `f42634387bd2fe7073feee002d3f9353dce41559`) added a direct-parent guard before that normalization. The production root-owner requirement, private-check profile and all verification limits are unchanged. The intermediate run `36305014241` was superseded/cancelled; final exact-functional run is `36305039955`, attempt 1. Final hosted validation succeeded on that exact functional head; its outcome and counts are recorded below.

Fresh disposable homes are used for durable/recovery tests; native sandbox/socket validation runs outside the outer tool sandbox because nested macOS confinement and local binding are unavailable there.


Local validation receipts (subprocess helper successes are not double-counted):

| Surface | Revision/scope | Result |
| --- | --- | --- |
| Bridge library | `ff415ebab`, serial, default thread stack | 704 passed, 0 failed, 1 ignored |
| Bridge integrations | `ff415ebab`, serial | 530 passed, 0 failed, 3 ignored; includes all 102 Verified Change workflow tests |
| Actual installed Grok CLI | `ff415ebab`, explicit ignored protocol test | 1 test passed with 3 cost-isolated success/refusal/cancellation cases |
| Private check authority | `6ef3e5d7`, native | All 23 unit tests passed, including operator-data/Data-volume-alias denial and Python startup |
| Service | Earlier `cc94c603` full run | 20 passed, 0 failed |
| Service | Latest `6ef3e5d7` local run | 14 passed, 1 baseline-reproduced failure; 5 smoke tests not reached |
| Formatting/clippy | Bridge and service through `6ef3e5d7` | Passed; clippy all targets with warnings denied |
| Desktop | Own isolated checkout; frontend unchanged by later Rust commits | Typecheck, 58 files/428 tests, production build passed |

Earlier local revisions do not replace final exact-functional hosted validation. The final successful exact-functional hosted results below cover the subsequent changes and additional Tauri/authority/pager/isolation surfaces.

Exact principal commands (bridge working directory unless stated):

```sh
cargo fmt --check
GROKPTAH_HOME=/private/tmp/rma-bridge-final-pass-home CARGO_INCREMENTAL=0 cargo test --locked -- --test-threads=1
GROKPTAH_HOME=/private/tmp/rma-clippy-final-home CARGO_INCREMENTAL=0 cargo clippy --locked --all-targets -- -D warnings
GROKPTAH_REAL_GROK_CLI=/Users/chriscase/.grok/downloads/grok-1.0.41-macos-aarch64 GROKPTAH_HOME=/private/tmp/rma-installed-final-home CARGO_INCREMENTAL=0 cargo test --locked --lib managed_provider::tests::installed_cli_obeys_production_protocol_and_failure_has_no_retry -- --ignored --test-threads=1
GROKPTAH_HOME=/private/tmp/rma-service-final-all-home CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/private/tmp/GrokPtah-real-managed-assignment-v1/crates/codegen/grokptah-agent-bridge/target cargo test --locked -- --test-threads=1
GROKPTAH_HOME=/private/tmp/rma-service-clippy-final-home CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/private/tmp/GrokPtah-real-managed-assignment-v1/crates/codegen/grokptah-agent-bridge/target cargo clippy --locked --all-targets -- -D warnings
npm ci
npm run typecheck
npm test
npm run build
git diff --check
```

Service commands run in `crates/codegen/grokptah-service`; npm commands in `desktop`. The qualification command is `GROKPTAH_HOME=/private/tmp/rma-live-host CARGO_INCREMENTAL=0 cargo run --locked --example real_managed_assignment -- ROOT /Users/chriscase/.grok/downloads/grok-1.0.41-macos-aarch64 MODE`, with modes readiness, execute, review, discard and two further review invocations. `execute` was never repeated after the live send. Apply was not run against the failed live result.

Failures preserved:

1. Outer sandbox prevented native check confinement/local socket binds; those commands were rerun with native authority, not weaker test confinement.
2. Early zero-send admission defects (bounds intersection; probe-root race) were fixed and their unsuccessful evidence preserved.
3. This implementation initially enlarged native provider futures enough to overflow the default Tokio thread stack. An exact integrated-base archive passed the same shutdown test. Heap allocation at the canonical transport boundary fixed the new regression; final tests use the normal stack. An intermediate enlarged-stack service run is not counted as final validation.
4. Service `disconnect_reconnect_restart_and_cursor_expiry_are_durable` intermittently filled the event-journal queue during shutdown and retained the lock. The exact `git archive 9e2660ff...` baseline reproduced the same failure with the same command and fixture. An earlier complete service run passed all 20 tests. The latest local run on `6ef3e5d7d` passed 14 tests and reproduced that same baseline failure; Cargo consequently did not execute the five smoke tests in that run. This baseline flake is preserved and is not silently classified as a new failure or repaired outside this slice.
5. Automatic approval review rejected an earlier proposed validation weakening. That edit was not applied. The safe alternative preserves the old strict parser and validates the actual production CLI's separate seven-field usage contract against host sends and durable journals. No rejected action remains pending.
6. The real provider-backed attempt failed closed. No model-auth, HTTP-status, completed-usage or zero-billing claim is inferred from that failure.
7. Overlapping local validation produced bounded native-startup/check timeouts (unsupported-version probe, qualified-network check, installed-CLI isolation before wire) and recurrence of the baseline journal-queue flake. The superseded full run was interrupted (exit 130); it is not counted as a passing suite. All time/confinement limits were preserved. The strict per-response revision `ff415ebab` passed the actual-CLI protocol test serially (5.84 s), all 704 library tests, and all 530 integration tests (including 102 Verified Change workflow tests). The canonical-Xcode revision passed all 23 check-authority unit tests locally. Final hosted results and exact-source qualifications are recorded separately.

Ignored gates are explicit: the actual-CLI protocol test is ignored by default but was run separately and passed; the legacy two-profile live dogfood and Computer Use/live composed test remain ignored because they would exceed this goal's one-attempt scope; the lock-holder helper is intentionally invoked by its parent subprocess test. No additional live provider or Computer Use dispatch was used.


## Completed exact-functional hosted validation

[Desktop run 36305039955](https://github.com/chriscase/GrokPtah/actions/runs/36305039955), attempt **1**, event **pull_request**, head **`97bf4d5a639650d68e45eaf68f52d66c17b22688`**, completed **success**, **0 failed steps**. The workflow explicitly checks out the PR head rather than a synthetic merge ref. `hosted-desktop-final.json` contains the independently retrieved full step receipt and test-summary counts.

The hosted image's selected `/Applications/Xcode_26.6.app` was initially uid 501/gid 20. The guarded CI step made it uid 0/gid 0 and removed group/world writes. Both that step and the complete private-check/bridge gate passed. This fixes the disposable validation environment while preserving the production root-owned public-bundle rule and all operator-data, Data-volume-alias, candidate-write and network denials.

| Hosted surface | Result on final functional revision |
| --- | --- |
| Frontend | Typecheck; 58 files/428 tests passed |
| Semantic Help contract/runtime/authority | 112 Rust tests passed; format/clippy/codegen/public bundle/provenance gates passed |
| Tauri desktop | 50 tests passed; production targets compiled |
| Bridge | 704 library + 530 integration tests passed, including 102 Verified Change workflow tests |
| Test gateway | 17 tests passed; format/clippy passed |
| Bridge combined | 1,251 passed, 0 failed, 5 ignored; child lock helpers not double-counted |
| Headless service | 20 passed, 0 failed; format/clippy passed |
| Shell | Shipped helper compilation passed |
| Pager composition | Shipped binary/minimal/render/PTY targets compiled; 1,050 library tests passed, 2 ignored |
| Host authority | 119 tests passed; format/clippy passed |
| Offline parity oracles | 7 selected tests passed; these repeat coverage rather than adding unique tests |
| Persistent-Agent certification | 84 tests passed, offline only |
| Adaptive Computer Use evaluator | 78 tests passed, synthetic authority only; no Computer Use dispatch |
| Coding worktree | 27 tests passed; format/clippy passed |
| Isolated surface | 135 full-suite tests plus 18 selected reruns passed; format/clippy passed |
| Oracle shape / provenance | Repo-root suite-shape and public-provenance checks passed |

Five default bridge ignores are preserved: actual installed CLI (explicitly passed locally against the cost-isolated fixture), two separately authorized live/CU dogfood scenarios (NOT RUN), child lock-holder helper (run by its parent), and the existing DurableWriteGuard doc example. The two pager ignores are existing visual frame inspection and the known accent-drift theme test. The full default suites otherwise passed. No extra live send or paid cancellation/retry was used to obtain CI success.

| Run | Exact head | Outcome / qualification |
| --- | --- | --- |
| 36295896756 | `81c8d08f14a61088e9a9adec6c6fef28b0611215` | Success on frozen-goal-only tip; not implementation qualification |
| 36302732773 | `cc94c603829ae54fbeb3a7050498b0c70c7641ee` | Superseded/cancelled |
| 36302996863 | `ff415ebab918c107c82fa1354f7d577b72a8980a` | Failed private Python check |
| 36303628757 | `6ef3e5d7d52935a69fcd47b031ed74027315134d` | Same failure after canonical-path correction |
| 36304334776 | `9af032760b0bad53707df715869c2a8de54d8f80` | Same failure; bounded fixture diagnostic retained |
| 36305014241 | `ecb9a731aa66ba280cb3c9e0fbcc69468a8414f5` | Superseded/cancelled by direct-parent guard |
| 36305039955 | `97bf4d5a639650d68e45eaf68f52d66c17b22688` | **Success**, attempt 1, exact-functional review qualification |

The final service success does not erase the separately documented local baseline-reproduced journal-queue failure. The final bridge success does not erase failed or interrupted earlier runs. Hosted success establishes this exact source's validation outcome, not a successful live software assignment.

## Provenance and review focus

Predecessor ancestry and preflight dispatch 0 are preserved. New branch/worktree is fresh and separate from both the primary checkout and #579. Historical source worktrees were not modified. No historical branch was merged or rewritten; donor commits: **NONE**. A local APFS copy of ignored build cache was used only to avoid rebuilding dependencies and is not code provenance. A disposable exact-main archive was used only for baseline comparison. No synced project source was edited.

This goal's safety gates were not weakened. Its requested successful live software change was not achieved. Remaining work requires diagnosing upstream acceptance with sanitized error evidence, then a **separately authorized new bounded qualification**, since this goal's paid attempt is spent. A new attempt must retain the same readiness/containment/isolation/check/publication gates. The CLI is deliberately supported only at the observed 1.0.41 contract on macOS; other versions/platforms fail closed. No dollar budget or usage completion is promised after an unaccounted request.

Independent review should focus on relay revocation/in-flight settlement, observer-before-wire accounting, exact Work/Run/Attempt recovery, hostile candidate execution under check revision 2, no source/secret leakage, and whether the qualification evidence supports the narrowly stated claims. The lack of a verified live candidate is the reason the overall result is partial. Do not merge PR #580, release, deploy or begin a follow-up goal from this handoff.
