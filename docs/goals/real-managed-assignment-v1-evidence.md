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


## Bounded abandoned-forward/diagnostics continuation — READY FOR INDEPENDENT REVIEW

This continuation implements Tasks A–C from the owner-supplied PR #580 assignment. Additional live inference requests: **0**. The original frozen G1–G10 goal remains **PARTIAL**; offline integration does not complete live G4/G5/G8. Independent acceptance is **NOT CLAIMED**. PR #580 remains draft and unmerged; no release or deployment occurred.

### Revision and publication boundary

| Identity | SHA | Tree |
| --- | --- | --- |
| Integrated #579/main base (unchanged) | `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d` | `f524b705a3b88cb895309506bf726e76c91874bf` |
| Previously reviewed functional | `97bf4d5a639650d68e45eaf68f52d66c17b22688` | `f42634387bd2fe7073feee002d3f9353dce41559` |
| Starting published evidence tip | `e8d841ce1dc287c1ccf03ad8b4b852a13a5cb7a3` | `193f1a6b988acae6c0868e1395739c2b11132269` |
| Repaired/tested functional | `fd8a7873468d7b416a53fc23b089b0cc4b589e2d` | `94d275b23090e873378e4690107a7d3abbba9061` |

The repaired commit is a normal descendant of the starting tip on the same `grok/real-managed-assignment-v1` branch. No reset, rebase, force push, main mutation or new PR was used. Its changes are restricted to `managed_provider.rs`, `grok_build.rs`, `orchestration/service.rs` and the new offline test module. All subsequent committed files are evidence/specification records under docs. **Executable changes after the tested functional revision: NONE.** The final publication SHA/tree and independently verified remote head are recorded in the PR body/handoff, avoiding a self-referential documentation commit.

Frozen goal content SHA-256 remains `50edf789c5acc46a0f349a81ca12b53ce155adfc860ff084537e0aba33f578f8`. All eleven historical artifact file digests match the starting manifest; the manifest only adds continuation records. The prerequisite preflight dispatch count remains **0**.

### A — Meaningful red reproduction and terminal repair

Before implementation, the actual production relay/canonical transport observed the first loopback upstream request, then the forward task was aborted before headers without calling revoke. Canonical authenticated reconciliation reported one uncertain attempt. Relay evidence had one admission, no completed response/usage, incomplete accounting and uncertainty, but `revoked=false`. A **different valid body using the same capability** then returned HTTP 200 and reached the fixture: two physical fixture calls/two authority admissions. The regression failed its assertion `2 == 1`: **0 passed, 1 failed**, not a setup panic. The private meaningful-red log digest is retained in [the reproduction record](real-managed-assignment-v1-artifacts/continuation-abandoned-forward.json). Earlier compile/setup failures are not substituted for this reproduction.

After repair, authorization and serialized admission both reject uncertain/revoked leases. The forward guard becomes completed only after durable canonical success settlement and the successful evidence transition. Every incomplete guard drop cancels/revokes the lease before clearing in-flight state. Both header-wait and response-drain barriers prove one uncertain canonical attempt, one admission/reservation, missing/incomplete usage, changed-body HTTP 401 and zero additional fixture calls. Another direct different-body admission also fails without reserving again. Stored token totals of zero mean unobserved usage; terminal authority makes them unusable as free budget.

Two distinct completely settled turns remain usable under a two-call bound, preserve exact 200-input/20-output accounting and do not revoke between turns. A third call is denied without another send. Actual relay socket disconnect is also covered: Hyper may drop the handler or retain it pending, but serialized admission prevents another send in either case. Quiescence/revocation does not undo a remote request. Durable settlement and the completed-marker update have no intervening await; the completed-turn and cancellation/expiry regressions cover this boundary without injecting a transport bypass.

All required names are present and executed: `dropped_forward_after_upstream_admission_revokes_capability`, `changed_request_after_abandoned_forward_causes_zero_additional_sends`, `unknown_usage_cannot_be_reused_as_free_budget`, `successful_settled_turn_preserves_bounded_multiturn_execution`, and the separately run installed-CLI `interrupted_forward_recovery_never_creates_another_paid_attempt`.

### B — Safe evidence, persistence and demonstrated protocol findings

The existing ManagedProviderEvidence/managed-intent/Work–Run projections now carry distinct reserved/admitted, HTTP-observed and usage-completed counts; numeric HTTP status; bounded request identifiers; allowlisted machine error enums; first interruption; and at most 32 typed diagnostics. Categories distinguish prewire denial, uncertain transport, observed HTTP/rejection, SSE/protocol failure, missing/inconsistent usage, child exit/metadata mismatch, cancellation, expiry, abandonment, settlement and uncertain recovery. Prewire denial retains a typed reason and bounded request-byte count. A proven before-wire transport failure may clear remote uncertainty without restoring authority. Legacy/lost-relay remote outcomes remain **unknown**, not invented false; canonical transport remains the sole send ledger.

Loopback fixtures cover 400/401/403/429/503, malformed SSE, missing and inconsistent usage, transport interruption and successful settlement. For example, the offline 401 retains `httpStatus=401`, `http_rejected`, validated UUID request ID and allowlisted `authentication_error`/`invalid_api_key`; its message, cookie and body are absent. Unknown/malformed machine codes and arbitrary request IDs are discarded. An HTTP rejection still has incomplete usage and remote uncertainty; it does not prove zero billing. Canary tests also reject a known credential masquerading as an otherwise allowlisted code/identifier.

Actual installed-CLI HTTP, stream, missing-usage and transport diagnostics survive finalization, explicit discard and three reopens with the same evidence and exactly one Work/Run/Attempt. Recursive scans of each owned fixture's retained files and public report found no injected response/transport/cookie/credential canary. Public reports omit prompt previews and free-form Run responses. See [HTTP fixture](real-managed-assignment-v1-artifacts/continuation-offline-http-rejected.json), [protocol fixture](real-managed-assignment-v1-artifacts/continuation-offline-malformed.json), [usage fixture](real-managed-assignment-v1-artifacts/continuation-offline-missing-usage.json) and [interrupted fixture](real-managed-assignment-v1-artifacts/continuation-offline-interrupted.json).

Installed CLI 1.0.41 request shapes are retained only as field/tool/parameter names, numeric byte lengths and output ceilings. The positive fixture sends the standard messages/model/tools/stream fields, a forced 1,024-token ceiling and usage-enabled SSE. A concrete installed-CLI mismatch was reproduced: two settled model calls and 220 tokens appear as headless `num_turns=2`, while `usage.json` records one user prompt (`turnCount=1`) containing both calls. The old equality falsely rejected it. The repaired validator requires exactly one prompt/usage row, exact host call/input/output/total counts in both session and prompt aggregates, exact headless model-round/call reconciliation and the existing journal/session/request binding. Validation was strengthened; token accounting was not replaced with a worker verdict.

[The primary Chat Completions reference](https://docs.x.ai/developers/rest-api-reference/inference/chat-completions) demonstrates an additional reasoning-token contribution to total usage. The pinned Grok headless guide documents cached versus uncached input and optional cost fields. Those nonzero reasoning/cache/additional-cost forms remain **unqualified** by the present fixtures; no parser broadening or live diagnostic request was used. [The coding model documentation](https://docs.x.ai/developers/models/grok-build-0.1) is capability context, not entitlement proof. These findings do **not** establish the historical failed request's cause, which stays **UNKNOWN**.

### C — Actual-CLI production-path OFFLINE qualification

The external model is a cost-isolating loopback fixture; the installed CLI, private child confinement, production relay, canonical transport, managed service/MCP, Work/Run/Attempt stores, retained candidate, registered private host oracle and explicit disposition/recovery are real. CLI 1.0.41 digest: `sha256:9c844eb13365180787d9ad22b2b3748a024be8e1ed845253cc114781b31c591d`.

Successful offline Work `b5eff315-201e-4a76-92e2-b634a68d25ab`, Run `f36dae6c-3051-4435-aba5-33c54fd14bde`, Attempt `3bc59042-4a51-4ec1-a585-eba2ac6f9e2f` (number 1), base `d10ec97b05d6b5f1643840b2233ad90c51ce74da`, tree `1ef9c0872b4eadfaae5f75bbf2933c062cce67ec`. Two actual CLI file-tool writes repair UTF-8 byte-length framing and canonical exact-length/strict-UTF-8 decoding in the two allowed source files. Two settled fixture calls total synthetic 200-input/20-output usage, within existing limits. Request sizes were 13,289 and 14,519 bytes. Earlier longer scripted transcripts correctly hit the existing token reserve and were shortened; no budget or confinement was enlarged.

Host oracle profile `offline-utf8`, revision 1, private confinement revision 2; sealed check specification `sha256:55d8ae3e2b6378a68634b1722509c3667a1a0d4ebcc59f18fdfaf085ca42478b`. Original registered check fails (exit 1), the retained candidate passes (exit 0). Candidate digest `sha256:53274f59ebf1ac3b4db81ec6c46f2d22c3e23359fc963183febb03c498775356`; diff bundle `sha256:22571f59ec41e6cfaae9d5ac29f0b4d0cc0fb3131164844e422941dd82312aa9`. Source remains clean at its base through review. A reopened host explicitly approves/applies that candidate on disposable source; two further reopens preserve applied/succeeded state, exact repaired bytes and one Work/Run/Attempt, with no further fixture calls and an empty worker root. [Complete sanitized positive record](real-managed-assignment-v1-artifacts/continuation-offline-success.json).

Interrupted offline Work `3e41b4a1-8c7c-4140-b9fe-4bfec183f9ee`, Run `96a28743-957b-49a1-9dbe-931d5e651200`, Attempt `90afbaae-578e-40e1-9840-268b292c637a` stays one attempt/one local fixture request through finalization, discard and three reopens; source remains unchanged and usage incomplete. This and the positive journey are **OFFLINE integration**, not live G4/G5/G8 completion.

### Validation and exact-functional Hosted Desktop

| Final campaign | Passed | Failed | Ignored / qualification |
| --- | ---: | ---: | --- |
| Locked bridge, fresh disposable home, default stack | 1,243 | 0 | 10; 713 library and 102 Verified Change recovery tests included |
| Bridge fmt / strict all-target Clippy | PASS | 0 | CARGO_INCREMENTAL=0; no stack override |
| Service fmt / strict Clippy / locked suites | 20 | 0 | 0 |
| Host authority fmt / strict Clippy / suites | 119 | 0 | 0 |
| Frontend typecheck / full tests | 428 | 0 | 58 files |
| Tauri native desktop | 50 | 0 | 0 |
| Installed-CLI managed journey/diagnostics, explicitly offline | 5 | 0 | 0; run separately from the normal campaign |
| Existing installed-CLI protocol/failure/no-retry fixture, offline | 1 | 0 | 0 |
| Affected check regressions rerun separately | 23 | 0 | 0 |

The first full bridge campaign (overlapping native qualification) had 706 passed, 7 timing/output/deadline failures and 6 ignored. Those failures are retained, not hidden. All 23 affected check regressions then passed unchanged, followed by the complete serial campaign above. No timeout, sandbox rule or test expectation was weakened. Intermediate development setup/transcript/metadata failures are qualified separately from the meaningful authority reproduction. [Exact commands and per-suite counts](real-managed-assignment-v1-artifacts/continuation-validation.json).

Hosted Desktop [36337674750](https://github.com/chriscase/GrokPtah/actions/runs/36337674750), attempt **1**, `pull_request`, exact functional head `fd8a7873468d7b416a53fc23b089b0cc4b589e2d`: **completed/success**, failed steps **0**. Its full workflow includes bridge/service/host-authority, Tauri, frontend, coding-worktree/isolated-surface, certification and provenance gates. [Bounded hosted metadata](real-managed-assignment-v1-artifacts/continuation-hosted-desktop.json). The subsequent single publication commit contains evidence/specification only; later CI metadata belongs in the PR body, not another docs commit.

### Historical preservation, next-live proposal and stop

Historical live Work `c724d7eb-d321-4159-af79-7861ad0bb10b`, Run `681606e4-b8a6-469f-ab80-ab246b861af7`, Attempt `752e00ee-5b62-4038-bbbc-3fd40a016878` is unchanged and cancelled. Its one failed live attempt/send and unknown/incomplete usage remain recorded; no retry, verified candidate or application has been invented. The earlier zero-wire isolation-failed Work is also not rearmed. Additional live inference/entitlement/paid diagnostic/cancellation requests in this pass: **0**.

[The next-live proposal](real-managed-assignment-v1-next-live-spec.md) is **NOT EXECUTED**. It pins CLI/model/OIDC proxy authentication/route, reproducible new red base and two allowed files, private external oracle, one worker attempt, no retry/application, 180-second lifetime, six maximum admissions, 16,000 aggregate tokens, 1,024 output tokens per call and current byte/tool ceilings. No new live qualification repository/Work/request ID exists. Independent review, complete accounting compatibility and separate precise owner authorization remain prerequisites. The frozen original goal stays PARTIAL. Stop after this publication for independent review; no merge, undraft, release, deployment or new goal is authorized.


## Completed-forward ownership continuation — READY FOR INDEPENDENT REVIEW

This bounded pass repairs the single completed-forward handoff finding. Additional live requests: **0**. The original live G1–G10 goal remains **PARTIAL**; independent acceptance is **NOT CLAIMED**. PR #580 remains draft and unmerged. The accepted abandonment, diagnostics and actual-CLI offline journeys are preserved within their stated fixture scope.

### Exact revision boundary

| Identity | SHA | Tree |
| --- | --- | --- |
| Integrated main / #579 base, unchanged | `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d` | `f524b705a3b88cb895309506bf726e76c91874bf` |
| Starting reviewed functional | `fd8a7873468d7b416a53fc23b089b0cc4b589e2d` | `94d275b23090e873378e4690107a7d3abbba9061` |
| Actual starting published tip | `80c8db55c8aeec39b2d5fbf49c92b42cf59f866e` | `b8d65b4409107bc86d8226b505c3324b84476f34` |
| Repaired/tested functional | `e70c5609628ef76847d9eceb02b0523a8980409d` | `d36d0f3b081a1b9e7448d27bf2a12b1f282c183b` |

Git/PR metadata was refreshed before implementation. Local and remote starting tips agreed; there was no intervening or unrelated local work. The repaired commit directly descends from the starting tip on `grok/real-managed-assignment-v1`. Its only changed file is `crates/codegen/grokptah-agent-bridge/src/managed_provider.rs`. The production repair is an early return in completed guard cleanup; other additions are test-only instrumentation and regressions. No ledger, generation subsystem, scheduler, credentials, isolation, accounting parser, budget or #579 machinery changed.

The final publication SHA/tree and independently checked remote head are recorded in the PR body/final handoff. One subsequent commit contains this evidence and sanitized JSON only. **Executable changes after the tested functional revision: NONE.** No reset, rebase, force push, main mutation, replacement PR, merge, undraft, release or deployment occurred.

### Meaningful before/after production-boundary reproduction

Four deterministic cases use a two-worker-thread runtime, the actual production relay/forward code and canonical transport, and a local upstream fixture. A one-use `cfg(test)` barrier pauses A after durable successful settlement and successful shared-state publication, after releasing the lease mutex and before its settlement guard drops. No lease/hook mutex is held at the barrier; no timing sleep establishes ordering. B must physically reach the fixture and remain before response headers/usage before A is released. Channel/notification waits use bounded three-second test deadlines. A uses the authenticated HTTP relay; uninterrupted B/C cases also use that relay. The successor-abort variant directly owns the same production forward future so task abortion deterministically drops B's guard; it does not equate client disconnect with Hyper handler destruction.

Before the production repair, the original Drop implementation was byte-identical to the starting code. A had one durably settled receipt (canonical pending count 0). B was then observed with two total reservations/admissions/fixture calls, one completed response and one pending canonical attempt. After A returned HTTP 200 and its completed guard dropped, B's active marker was **false** and `provider_quiescent` was **true**, despite that outstanding B attempt. A distinct valid C returned **200**, taking all three counts to **3**. The meaningful failures were the active-marker assertion, false-quiescence assertion, successor-ownership assertion and third-count assertion `(3,3,3) == (2,2,2)`: **13 passed, 4 failed, 1 ignored**, not a setup/permission panic. This demonstrates lost serialization within the existing finite request cap; it does not imply unlimited spending.

After repair, completed `ForwardSettlement::drop` returns before locking or mutating shared lease state. Its successful transition has already released that forward's ownership. The existing incomplete cleanup remains unchanged: interruption/uncertainty, revocation and token cancellation precede releasing in-flight admission under the same mutex. The canonical transport remains the only send authority.

| Assertion | Before | After |
| --- | --- | --- |
| A durably settled before B admission | 1; pending 0 | 1; pending 0 |
| B physically observed and canonical pending | 1 pending B | 1 pending B |
| B active after completed A cleanup | false | **true** |
| Quiescence while B remains outstanding | true | **false** |
| Entire B evidence unchanged by A cleanup | yes; active marker was lost | **yes, with active ownership retained** |
| Distinct C status while B is active | 200 | **502 prewire refusal** |
| Total reservations / canonical admissions / fixture sends after C | 3 / 3 / 3 | **2 / 2 / 2** |
| Uninterrupted A/B completion | ownership assertion failed | **2 settled turns; 200 input / 20 output; accounting complete; quiescent; not revoked** |
| Aborting actually active B after A completes | ownership assertion failed | **revoked; abandoned_forward; one pending canonical B; usage incomplete** |

The C-denial variant preserves the current policy: refusal revokes/cancels B. It joins B's resulting failure while the upstream fixture remains running, so fixture shutdown cannot substitute for cancellation. The uninterrupted success variant is separate. After explicit B task abortion, only A's settled 100-input/10-output totals remain; B's usage stays unknown, not zero-cost. Changed-body HTTP C returns 401, another direct changed-body admission is refused, and all counts stay 2. Local quiescence after B cleanup does not undo the remote request.

Focused final relay result: **17 passed, 0 failed, 1 ignored**. All four new regressions pass: `completed_forward_drop_cannot_clear_successor_inflight`, `completed_guard_drop_does_not_publish_false_quiescence`, `successor_inflight_prevents_a_third_forward_after_predecessor_drop`, and `completed_predecessor_preserves_successor_abandonment_revocation`. Existing header-wait/body-drain abandonment, socket-disconnect, cancellation/expiry, bounds, safe diagnostics, unknown usage and two-settled-turn tests also pass unchanged. [Observed events, commands, source/log digests and exact assertions](real-managed-assignment-v1-artifacts/completed-forward-reproduction.json).

### Final validation and preserved offline qualification

All local native campaigns ran serially, with `CARGO_INCREMENTAL=0`, a normal stack and disposable host homes. The final locked bridge suite used a fresh `GROKPTAH_HOME` and the established `--test-threads=1` command. No timeout, confinement, usage expectation or limit was widened.

| Campaign on the functional tree above | Passed | Failed | Ignored / qualification |
| --- | ---: | ---: | --- |
| Bridge full locked suite | 1,247 | 0 | 10; 717 library and 102 Verified Change tests included; 39 top-level suites |
| Bridge fmt / strict all-target Clippy | PASS | 0 | Normal stack |
| Service fmt / all-target check / strict Clippy / isolated tests | 20 | 0 | 0 |
| Host authority fmt / strict Clippy / locked suite | 119 | 0 | 0 |
| Frontend typecheck / complete tests | 428 | 0 | 58 files |
| Tauri Rust tests | 50 | 0 | 0 |
| Actual installed-CLI managed/diagnostic cases, separately OFFLINE | 5 | 0 | 0 |
| Existing installed-CLI protocol/cancellation/no-retry fixture, OFFLINE | 1 | 0 | 0 |

The deliberate four pre-fix failures above are retained as reproduction evidence. No unexpected setup/baseline/transient validation failures occurred in this repair campaign. Earlier seven timing/output/deadline failures, their unchanged 23-check rerun and the prior passing serial campaign remain in the unchanged predecessor validation receipt/section; their cause is not retroactively reclassified. [Exact commands, per-suite counts and failure qualifications](real-managed-assignment-v1-artifacts/completed-forward-validation.json).

Installed CLI 1.0.41 digest remains `sha256:9c844eb13365180787d9ad22b2b3748a024be8e1ed845253cc114781b31c591d`. All five managed journey/diagnostic cases and the separate existing protocol fixture were rerun against local upstreams, with accepted private confinement, production relay/canonical authority, Work/Run/Attempt stores, retained candidate and host-owned oracle. The positive red-to-green journey still requires explicit application on disposable source; failure cases remain discardable without source mutation. Three reopens preserve evidence, one Work/Run/Attempt and unchanged fixture counts. The fixture responses are not proof that a live model solved the assignment. [Positive rerun](real-managed-assignment-v1-artifacts/completed-forward-offline-success.json), [HTTP rejection](real-managed-assignment-v1-artifacts/completed-forward-offline-http-rejected.json), [interruption](real-managed-assignment-v1-artifacts/completed-forward-offline-interrupted.json), [protocol](real-managed-assignment-v1-artifacts/completed-forward-offline-malformed.json), [missing usage](real-managed-assignment-v1-artifacts/completed-forward-offline-missing-usage.json).

Hosted Desktop [36343929733](https://github.com/chriscase/GrokPtah/actions/runs/36343929733), attempt **1**, `pull_request`, exact head `e70c5609628ef76847d9eceb02b0523a8980409d`: **completed/success**, failed steps **0**. [Exact-functional hosted metadata](real-managed-assignment-v1-artifacts/completed-forward-hosted-desktop.json). The later evidence-tip run is separate; any later CI metadata belongs in the PR body rather than another documentation commit.

### Historical preservation and stop

All **20** starting receipt files remain byte-identical, including the original **11** historical artifacts and all nine accepted continuation records. The digest manifest only adds this pass's records. Frozen goal SHA-256 remains `50edf789c5acc46a0f349a81ca12b53ce155adfc860ff084537e0aba33f578f8`. The next-live specification is also byte-identical, SHA-256 `d731eb90e391047826dfe1e14d4d0600e338308a635bf94fab9bb8a1d6121e41`. Canary/credential-shape scans and preservation results are in [the bounded preservation record](real-managed-assignment-v1-artifacts/completed-forward-preservation.json).

Historical live Work `c724d7eb-d321-4159-af79-7861ad0bb10b`, Run `681606e4-b8a6-469f-ab80-ab246b861af7`, Attempt `752e00ee-5b62-4038-bbbc-3fd40a016878` is not rearmed or modified. Its one failed child/recorded live send, unknown/incomplete usage and **UNKNOWN** cause remain preserved. Preflight dispatch count remains **0**. Additional inference, entitlement, paid diagnostic, live cancellation or auth-route/model fallback requests in this pass: **0**. No new live qualification root/Work/request ID was created.

The [next-live specification](real-managed-assignment-v1-next-live-spec.md) remains **NOT EXECUTED**. Nonzero reasoning/cache/additional-cost accounting compatibility and separate precise owner authorization remain gates. This repair neither broadens accounting nor completes original live G4/G5/G8. Stop after this single publication for independent review. Independent acceptance is **NOT CLAIMED**; no merge, undraft, release, deployment, unrelated hardening or new goal is authorized.


## Accounting compatibility continuation — READY FOR INDEPENDENT REVIEW (OFFLINE ONLY)

This bounded pass resumes from published tip `4aff414435c26d3767dca245baf6493794e589db` (tree `f0dd6216109bae241808280eeca97fc37312faba`). The accepted completed-forward functional revision `e70c5609628ef76847d9eceb02b0523a8980409d` remains an ancestor and its ownership repair is unchanged. Integrated main remains `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d` (tree `f524b705a3b88cb895309506bf726e76c91874bf`). New tested functional revision: `0be8ec4b32144d002d8bb230dec78d18d018f922` (tree `ad0c6d0a500d309981c3eddbee2531625106b89c`). Only the managed relay/evidence, installed-CLI reconciliation and offline fixture files changed in that commit. The eventual publication commit adds this section and JSON receipts only; its final SHA/tree and the exact functional-to-evidence diff are recorded in the PR body/handoff. Executable changes after the tested functional revision: **NONE**.

### Source contract and decision boundary

[Chat Completions](https://docs.x.ai/developers/rest-api-reference/inference/chat-completions) publishes prompt, completion, reasoning detail and total fields, including an additive reasoning example. [Prompt caching](https://docs.x.ai/developers/advanced-api-usage/prompt-caching/usage-and-pricing) treats cached tokens as a subset of full prompt. [Cost tracking](https://docs.x.ai/developers/cost-tracking) specifies exact `cost_in_usd_ticks` units; the pinned installed-CLI headless guide separately projects cached/uncached input and optional complete/partial cost. These sources establish why the host must distinguish full prompt, cache detail, reasoning detail, total and cost; they do not prove every public API shape is compatible with installed CLI 1.0.41. [Normalized contract and complete path map](real-managed-assignment-v1-artifacts/accounting-contract.json).

The canonical managed receipt now stores full prompt and completion tokens, exact total, cache and reasoning subsets and optional exact positive cost ticks. Cache is never subtracted from the full-prompt budget or double-added to total. Reasoning is accepted only when it is a completion subset and the provider's total equals prompt plus completion, matching the installed CLI's current headless projection. The relay parses the final SSE usage before canonical success settlement; checked aggregation and the total-token reservation drive later admission. The installed CLI stdout matcher reconciles uncached input plus cache against full prompt, reasoning and exact ticks/derived USD, with the one-prompt session row and physical model-call count. Durable Work/Run/Attempt review and three reopens retain the same host evidence.

The public additive-reasoning-total shape is deliberately **terminal fail-closed** because CLI 1.0.41 cannot project it exactly under its current headless total contract. Unknown additional usage/cost keys, nonzero unsupported detail/source categories, contradictory cache/reasoning/aggregate values, mixed priced/unpriced turns and arithmetic overflow also fail closed before success settlement. The canonical attempt remains pending/uncertain, the lease is revoked, and no second physical send or budget refund occurs. Missing cost in an otherwise exact token receipt is explicitly `costComplete=false`, `costInUsdTicks=null`—**not** a zero-dollar charge. The frozen host uses a token ceiling and specifies no dollar ceiling, so ordinary input/output-only behavior remains valid with a distinct unknown monetary value; no price is inferred. An actual extra charge or usage extension may never be discarded as free.

### Meaningful red to green

Against the starting production code, relay/canonical fixture regressions produced **17 passed, 4 failed, 1 ignored**. Combined nonzero reasoning/cache/cost had HTTP 200 but durable `reasoningTokens=null` instead of 3; cache>prompt, reasoning>completion and an unknown extra usage field each had HTTP 200 instead of required terminal 502. Separately, the installed-CLI matcher rejected a valid uncached-input/cache/reasoning/exact-ticks fixture (**0 passed, 1 failed**). These are assertion failures after actual boundary execution, not setup failures. The same focused relay suite now has **29 passed, 0 failed, 1 ignored**, and the CLI matcher passes. [Before/after assertions and log hashes](real-managed-assignment-v1-artifacts/accounting-reproduction.json).

### Final-tree validation

Bridge fmt/strict all-target Clippy passed. Locked bridge, fresh home, normal stack and serial execution: **1,260 passed, 0 failed, 13 ignored** across 39 top-level suites, including 730 library and **102 Verified Change** tests. Service fmt/check/strict Clippy passed; host-authority fmt/strict Clippy and **119 tests**, frontend typecheck and **428 tests/58 files**, and Tauri **50 tests** passed. The existing installed CLI 1.0.41 ran eight managed scenarios **OFFLINE** against local loopback fixtures, plus its separate protocol/cancellation/no-retry fixture: **8/8 + 1/1**. No provider request or operator credential was used.

The first final-tree service run had **10 passed, 1 failed** in unchanged `disconnect_reconnect_restart_and_cursor_expiry_are_durable`: shutdown reported `journal writer queue is full` and retained its temporary lock. The unchanged case passed in isolation, then the full serial service suite passed **20/20** with a fresh home. This failure is retained in [validation evidence](real-managed-assignment-v1-artifacts/accounting-validation.json); no timeout, assertion, limit or code was weakened. Accepted completed-forward ownership, abandonment/no-retry, bounded diagnostics, physical-send authority and Verified Change regressions are green in the final bridge and offline campaigns.

The combined installed-CLI journey settled two synthetic local fixture sends with 200 full input, 20 output, 220 total, 60 cached-input subset, 7 reasoning-output subset and 1,665 exact positive cost ticks; candidate, host oracle and explicit disposable-source application succeeded and three reopens retained the evidence. The original zero-cache ordinary journey also settled two fixture sends and retained 200/20/220 tokens but kept monetary cost **unknown**, never zero. Additive reasoning and unknown extra-charge shapes each stopped after one fixture request, with no candidate/application, no retry and three stable reopens. Missing/malformed/HTTP-rejected/interrupted journeys remain one physical request each. [Eight sanitized offline reports](real-managed-assignment-v1-artifacts/accounting-preservation.json) and their digests are in the manifest.

**Exact-functional Hosted Desktop:** [run 36364316154](https://github.com/chriscase/GrokPtah/actions/runs/36364316154), attempt 1, `pull_request`, head `0be8ec4b32144d002d8bb230dec78d18d018f922`, **completed/success**, zero failed steps. [Hosted metadata](real-managed-assignment-v1-artifacts/accounting-hosted-desktop.json). The 29 predecessor artifacts are byte-identical; frozen goal and next-live spec hashes are unchanged. New reports passed bounded credential-shape/canary scans; see [preservation record](real-managed-assignment-v1-artifacts/accounting-preservation.json). The original historical failed live Work/Run/Attempt and UNKNOWN usage/cause remain untouched. Additional live requests: **0**; next-live specification: **NOT EXECUTED**; no new live repository/root/Work/request ID.

This offline accounting gate is **READY FOR INDEPENDENT REVIEW** under the goal's definition: each covered shape is exactly represented or explicitly terminal fail-closed. Live compatibility and original frozen G1–G10 completion remain **PARTIAL**; no independent acceptance or next live dispatch is claimed. PR #580 remains draft and unmerged pending owner decision.


## Authorized live qualification — FAILED (2026-09-28)

The owner independently accepted the offline accounting gate and authorized **one** new bounded live managed-worker attempt on the accepted executable revision `0be8ec4b32144d002d8bb230dec78d18d018f922` (tree `ad0c6d0a500d309981c3eddbee2531625106b89c`), starting from published evidence tip `7e0d4b85875c8de33ae1e78b0c48cd38d7dd7498` (tree `fe88bf8d4db0cd51ba2ef9491ac954874d777278`). Exact-functional Desktop [36364316154](https://github.com/chriscase/GrokPtah/actions/runs/36364316154), attempt 1, succeeded. Current main remained `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d`. No executable file changed. The earlier next-live proposal's accounting-review and owner-authorization prerequisites were fulfilled for this attempt; its historical "NOT EXECUTED" sections remain accurate as historical records.

The installed native Grok CLI was **1.0.41**, SHA-256 `9c844eb13365180787d9ad22b2b3748a024be8e1ed845253cc114781b31c591d`. The production qualification example built from the accepted tree had SHA-256 `26aa9955c99c741afb30522e56c6ba76121bc7e0b1f8450b6e4f0b52222142be`. A local, no-network attestation with at least ten minutes of credential validity confirmed Grok Build OIDC, the exact CLI-proxy route, no override or conflicting API-key source, and confined auth-file metadata. It did not print or publish credential material. No entitlement, model, fallback or diagnostic request was made.

A new disposable repository `/private/tmp/rma-live-qualification-v1-eYvGhSu2` was created from the prepared red two-file fixture. Actual base SHA `adaa04a2ff0b57dafd5eae69810c4eb034c89bc1`, tree `1ef9c0872b4eadfaae5f75bbf2933c062cce67ec`; both tracked files have mode `100644`, there are no remotes, and allowed mutation paths are exactly `src/framing.py` and `src/decoder.py`. The prepared spec's example root commit identity was a proposal; this actual commit has the same specified tree and was frozen before admission. The external oracle `utf8-frame@1` used `check.py` SHA-256 `ea6453adb05e49fdf427e304565f024f816569e4bbccf85b095816bf056a0032`, `check.sh` SHA-256 `b04ed3bb317eaa1a038939409c39a005253fcb230fbd86feb65a7d9895c668c0`, directory digest `sha256:28d268ff47be8af539b85a7a0dbc5d3965f41715424be169bc8ecbdb3e8e98fe`, private confinement revision 2, no network, five-second check timeout and the existing output ceilings. Its host-run red baseline failed with exit 1. Authenticated production prepare then returned READY, zero worker attempts and zero provider invocations with the pinned 180-second, six-request, 16,000-token and 1,024-output limits, twelve-tool cap, 64-KiB request and 256-KiB response ceilings, zero automatic retries, and no child publication or source-application authority. [Readiness receipt](real-managed-assignment-v1-artifacts/live-qualification-20260928-readiness.json).

Exactly one authenticated production start used the original prepare request ID `0f477d2f-fe56-4001-9018-332bbe1d9dda`. It created Work `3067ceb3-3132-48c9-8241-9267c109ae9b`, Run `25ba88cc-92b6-4afb-810d-23c81625b9f4`, Attempt `6fc4ec50-b921-41cf-96c4-df97cc0a21ae`. The production relay/canonical send authority reserved and admitted **one** physical request and observed **HTTP 403**. There was no safe provider error code, type or request ID. The response did not produce a complete usage receipt: completed responses **0**, reported total tokens **UNKNOWN**, monetary cost **UNKNOWN**, cache/reasoning usage unobserved, accounting incomplete and remote effect uncertain. Numeric zero counters are **not** proof of zero billing. The capability was revoked; no retry or second request occurred. The canonical ledger records one `settled` / `response observed` attempt, zero reconciliation-pending attempts and zero active leases after clean ordered shutdown. The provider's specific rejection reason is **UNKNOWN**; HTTP 403 alone does not establish entitlement, authentication cause or zero charge. [Sanitized outcome receipt](real-managed-assignment-v1-artifacts/live-qualification-20260928-outcome.json).

No two-file repair or candidate was retained (`sha256:missing`); the required candidate check is `invalidated`, not green, and the Work is in `review`, **not** awaiting approval. Source HEAD/tree and both source bytes remain unchanged; its Git status is clean and the worker root is empty. Three separate local authenticated review/reopen invocations preserved the same Work/Run/Attempt, attempt count 1, one admitted request, zero completions, revoked/uncertain state and missing candidate without any further dispatch. No approve, apply or discard operation was invoked. The success criteria for meaningful repair, candidate identity, green host oracle, complete token reconciliation and awaiting-approval review are unmet.

All **42** predecessor artifact digests and the frozen goal/next-live specification bytes remain unchanged. Six private retained JSON reports and five local command logs were checked against exact current cached OIDC access/refresh token bytes and bounded credential/header patterns: **zero matches**. Only typed allowlisted facts are published; raw upstream response, child transcript, bearer/cookie values and private custody files are excluded. [Preservation and scan scope](real-managed-assignment-v1-artifacts/live-qualification-20260928-preservation.json). The original historical failed live Work `c724d7eb-d321-4159-af79-7861ad0bb10b` and its UNKNOWN usage/cause remain unchanged. The final evidence-only publication SHA/tree and remote PR head are recorded in the PR body/final handoff to avoid a self-referential commit.

**Outcome: LIVE QUALIFICATION FAILED.** PR #580 remains draft and unmerged. The next action is to resolve the HTTP 403 access question out of band with the owner/provider; any further provider attempt requires new precise authorization. This attempt cannot be retried or turned into a parser change under this authorization. No release, deployment or new goal follows.

### 2026-09-28 zero-provider-request HTTP 403 diagnostic

A [bounded offline request-parity audit](real-managed-assignment-v1-http-403-diagnostic-20260928.md) traced the accepted managed OIDC forward and canonical transport, inspected the pinned local CLI identity and public xAI source, and exercised two synthetic focused probes ([typed evidence](real-managed-assignment-v1-artifacts/http-403-parity-20260928.json), [test-only patch](real-managed-assignment-v1-artifacts/http-403-offline-probes-20260928.patch)). It confirmed the relay omits `x-grok-model-override` and other native-style identity/affinity headers, while current public xAI documentation says non-default models route by model-override header. The public source revision is not proven to match installed CLI 1.0.41; the historical request has no retained wire-header trace or safe provider rejection detail. **Classification: INSUFFICIENT PINNED CONTRACT OR RETAINED EVIDENCE.** The original HTTP 403 cause and usage/cost remain UNKNOWN. Additional live provider requests, credential/account mutations and production executable changes: **0**. PR #580 remains draft and unmerged; no failed Work was retried or altered.

## Accepted model-routing repair: bounded live qualification — FAILED (2026-09-28)

The owner authorized one fresh live worker attempt on functional `4771f0e0831e0c0b8d50b2ebf661582dfd02db41` (tree `3f4926f803af3f9d003ff74a1c70be5ee0f0ec57`) and conditional integration only after complete qualification. The starting evidence commit was `5cf66d690411437dd18f232c57e491ffe9738343`; GitHub and local Git both identify its tree as `55f03f3d50ab83804abce86b7c0087869b2333d7`, whereas the assignment listed `443f5a745247c7638c862316ff31f021c1d15dc94`. Its SHA and six documentation/evidence-only changed paths are correct; the tree string in the assignment was mistaken. Main remained `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d`. Exact-functional Hosted Desktop [36495162391](https://github.com/chriscase/GrokPtah/actions/runs/36495162391), attempt 1, passed with zero failed steps. No executable source changed after the accepted functional revision. The offline-built production qualification executable was SHA-256 `a8622f3d0971c4924a05aeacf26a1130c68a2b1005fd2c11b4d7cbb242fd4bf0`; the exact installed Grok CLI 1.0.41 remained SHA-256 `9c844eb13365180787d9ad22b2b3748a024be8e1ed845253cc114781b31c591d` (revision label `4220f3b224a6`).

The new disposable repository `/private/tmp/rma-live-qualification-v1-k6lp7u` had base `e200ec5bc3d5968a5c66d96446a09c15264bda37`, exact expected tree `1ef9c0872b4eadfaae5f75bbf2933c062cce67ec`, only `src/framing.py` and `src/decoder.py` at mode `100644`, and no remote. The external `utf8-frame` oracle retained `check.py` SHA-256 `ea6453adb05e49fdf427e304565f024f816569e4bbccf85b095816bf056a0032`, `check.sh` SHA-256 `b04ed3bb317eaa1a038939409c39a005253fcb230fbd86feb65a7d9895c668c0`, directory digest `28d268ff47be8af539b85a7a0dbc5d3965f41715424be169bc8ecbdb3e8e98fe`, private confinement revision 2 and no network. Its host-controlled red baseline failed with exit 1. A local-only attestation verified the confined, first-party Grok Build OIDC record, official CLI-proxy route, absent credential overrides and more than ten minutes of validity without login, refresh or entitlement traffic. Production prepare returned READY with **zero** Work, worker attempts and provider invocations. The 180-second lifetime, six-request and 16,000-token ceilings, 1,024-output-per-request cap, twelve-tool limit, request/response ceilings, no retry and no source application remained bound. [Sanitized readiness receipt](real-managed-assignment-v1-artifacts/model-routing-live-20260928-readiness.json).

The first live-start tool escalation was rejected before process launch because automatic approval review required more specific payload and destination evidence. The checked fixture contains only the two public repository files above, the fixed UTF-8 framing repair objective and the specified `https://cli-chat-proxy.grok.com/v1/chat/completions` model route. The same bounded execute invocation was then approved; the rejected invocation created no Work or dispatch. The approved invocation started **once**, creating Work `b3b46d7c-0435-4781-a1f3-b7674a644526`, Run `7dd1deb7-6619-42ab-bd51-25cf96886e93` and Attempt `3f7799a9-6b46-468b-b0da-5e9c7da686e0`.

The relay reserved and physically admitted **one** upstream request and observed **HTTP 200** with a validated safe provider request ID. It then recorded `usage_inconsistent` and `child_exit`; **zero** responses acquired an accepted usage settlement. The canonical send audit has one `send_intent`, one `send_wire_admission`, and an `uncertain` outcome; its attempt is `uncertain` / `ProtocolAfterPossibleEffect` with zero active leases after shutdown. The host Run is `interrupted` with `token_accounting_unavailable`; the Work is in durable `review`. Reported input, output, total, cache, reasoning usage and monetary cost are **UNKNOWN**, even though some incomplete projection counters display zero. No complete host/CLI/journal token reconciliation exists. The lease is revoked, remote effect remains uncertain, and there was **no retry, fallback, second worker attempt or further provider request**. HTTP 200 does not establish task success, a valid usage contract, or retrospective cause of the earlier HTTP 403. [Sanitized outcome receipt](real-managed-assignment-v1-artifacts/model-routing-live-20260928-outcome.json).

No two-file repair or candidate was retained (`sha256:missing`); the required candidate oracle was `invalidated`, not green. Three sequential **local-only** reopens kept the same Work/Run/Attempt, one wire admission, one HTTP response, zero completed usage responses, revoked/uncertain state and missing candidate without redispatch. The immediate post-run generic stop reason resolved on first reopen to `max_total_tokens_usage_unavailable`; the latter two reopens were stable. Source HEAD/tree, index digest, both file digests, clean status and absence of remotes remained identical to the pre-dispatch snapshot. The worker root was empty. No approve, apply or discard action occurred; the historical failed Work/Run/Attempt and its UNKNOWN usage/cost/cause were untouched. [Sanitized preservation receipt](real-managed-assignment-v1-artifacts/model-routing-live-20260928-preservation.json).

**Outcome: LIVE QUALIFICATION FAILED.** The complete accounting, retained candidate, green oracle and awaiting-approval criteria are unmet, so the conditional merge gate is closed. PR #580 remains draft and unmerged. The new evidence is documentation only; no parser, limit, model, endpoint, credential or production behavior was changed. No further provider request, release, deployment or new goal follows from this attempt.

## HTTP-200 usage rejection diagnostic continuation — OFFLINE ONLY (2026-09-28)

This pass began at published `085f57cfef20bf0f2dddced657c08538b47b138e` (tree `af002fa2d85057f8bee6914a3371ee45b7b7456d`) on the existing draft PR #580. The accepted live-executed functional revision `4771f0e0831e0c0b8d50b2ebf661582dfd02db41` remains an ancestor, and integrated main remains `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d`. The new functional revision is `88a1b893715aec370e3ec6f542a18729fffe6592` (tree `500ddfb47ed859209a872c6959bded7c9cfcd44f`). Only the managed relay and its offline tests changed in that revision; this subsequent evidence record adds no executable change.

### Recovered observation and decision

The three existing model-routing-live receipts and 22 exact-root retained private reports, transcript, orchestration and canonical authority records were inspected locally. They establish one admitted request, HTTP 200, broad `usage_inconsistent`, zero accepted usage settlements and an `uncertain` canonical outcome. They **do not retain the local 502 `error.message`, the rejected usage object, or the exact failing field/value**. The host transcript has zero rows, and the inspected records contain none of the executed static managed usage/stream/response rejection reasons. The canonical audit retains `ProtocolAfterPossibleEffect`, not the parser reason. The later `max_total_tokens_usage_unavailable` is a downstream stop reason. Thus the actual condition remains **UNKNOWN**; the only established boundary is after HTTP 200 and before accepted accounting, within validation or aggregation. No synthetic value below is presented as the historical response.

Because the historical input shape and correct accounting semantics cannot be established, **usage acceptance was not changed**. No historical subreason was guessed or backfilled. The broad diagnostic categories remain compatible; new failures attach a typed `usageRejection` within the existing durable `providerEvidence.diagnostics` path. The typed record distinguishes missing/repeated receipts, missing/malformed/unsupported fields or details, conflicting totals/subsets, unsupported cost representations, and aggregation failures. It retains only allowlisted identifiers, value type/presence and bounded numeric observations. Unknown provider keys/prose and credential canaries are excluded. Rejected observations never become settled tokens or cost, replenish a reservation, or restore a lease. An old record with no subreason remains **UNKNOWN**. [Sanitized decision and validation receipt](real-managed-assignment-v1-artifacts/usage-rejection-offline-20260928.json).

### Independent synthetic checks and preservation

The diagnostic matrix and actual installed Grok CLI 1.0.41 (`sha256:9c844eb13365180787d9ad22b2b3748a024be8e1ed845253cc114781b31c591d`) were exercised **OFFLINE** with synthetic loopback responses, disposable homes/credentials and host-controlled confinement. Eight installed-CLI journeys passed, including positive ordinary and combined accounting, interrupted and HTTP/protocol failures, missing usage, unknown extra usage, and additive reasoning. Failure journeys retained typed evidence through three local reopens without retry or source mutation. The focused managed-provider suite passed 35 tests, with nine installed-CLI tests ignored by default; strict all-target Clippy, formatting and diff checks passed. The full bridge suite passed **736 tests, zero failed, nine ignored** serially with a disposable application home. An initial misdirected workspace-root test build failed first at sandboxed `protoc` and then, under native build authority, at a linker disk limit; its generated target was cleaned before package-scoped validation. An initial parallel bridge run failed 13 tests due to shared test-environment interference; a serial run against the default home failed three unrelated provider-qualification tests due to stale authority state. Those non-green runs are retained in the linked receipt rather than counted as passes.

Exact-functional Hosted Desktop [run 36518345360](https://github.com/chriscase/GrokPtah/actions/runs/36518345360), attempt **1**, `pull_request`, head `88a1b893715aec370e3ec6f542a18729fffe6592`, completed **success** with **zero failed steps**. The subsequent publication is evidence only; its final SHA/tree are recorded in the PR handoff.

The historical Work `b3b46d7c-0435-4781-a1f3-b7674a644526`, Run `7dd1deb7-6619-42ab-bd51-25cf96886e93` and Attempt `3f7799a9-6b46-468b-b0da-5e9c7da686e0` remain untouched, with usage and monetary cost **UNKNOWN**, one prior wire admission, revoked authority and no candidate. This pass made **zero additional live provider requests** and performed no approval, application or discard. This is diagnostic readiness for a future response; it does not establish historical billing, repair the failed qualification, or authorize another live attempt.

**Status: DIAGNOSTICS READY — HISTORICAL USAGE SHAPE UNAVAILABLE.** PR #580 remains draft and unmerged for independent review. No release or deployment follows from this pass.

## One-request live usage diagnostic — DIAGNOSTIC INCONCLUSIVE (2026-09-29)

The owner authorized one fresh instrumented managed-worker request on accepted functional `88a1b893715aec370e3ec6f542a18729fffe6592` (tree `500ddfb47ed859209a872c6959bded7c9cfcd44f`), starting from evidence-only tip `6c62c2209aa06b5f1eb9b1ec7625c17990e00c8f` (tree `f9ca94eddeba38c734b3a02efc76fa7f8a6b462d`). The executed repository source was unchanged. A temporary wrapper around the production prepare/start/status example narrowed the assignment to **one round and one physical request admission**; its source SHA-256 was `fb4f0682f8bc03a5c06372ec4def1501af6dd707e7661c77eb29c5e976892a44` and the live binary SHA-256 was `ed5242fc5dc103281fdec1d7e09c6515c4abdc181e6875175082b5f6f4f7d8ef`. The installed Grok CLI remained 1.0.41, revision label `4220f3b224a6`, SHA-256 `9c844eb13365180787d9ad22b2b3748a024be8e1ed845253cc114781b31c591d`, on model `grok-build-0.1` and the accepted official OIDC CLI-proxy route.

The disposable remote-free two-file fixture had tree `1ef9c0872b4eadfaae5f75bbf2933c062cce67ec`, unchanged external oracle bytes and a failing host-owned baseline. Local-only credential attestation passed without refresh or account calls. Production readiness reported READY, **zero** worker dispatches and **zero** provider invocations. The sealed limits were one round/request, 180 seconds, 16,000 aggregate tokens, 1,024 output tokens per request and 16,000 prompt bytes; the existing tool and response ceilings and no-retry/no-application rules remained. A focused synthetic relay test passed before dispatch, proving the tighter assignment bound cannot be amplified into a larger credential lease.

The sole start created Work `f619e7b1-6c94-4dc1-aab7-39db76db8362`, Run `148c1326-5148-42a6-ae6b-2f0f21272efe` and Attempt `911398d1-d62a-4ec4-a2ed-56a300447d1c`. The relay reserved and physically sent **one** request. It observed HTTP **200** and safe provider request ID `80bbfbca-16ef-9e7d-b4de-b507014cb104`, then emitted `usage_inconsistent` with typed `usageRejection`: `conflicting_total`, field `total_tokens`, observed **3,946**, expected prompt-plus-completion **3,614** (difference **332**). No response received accepted usage settlement. The canonical log has one intent, one wire admission and one `uncertain` / `ProtocolAfterPossibleEffect` outcome. The capability is revoked, remote effect uncertain, accounting and cost incomplete. Total usage and monetary cost are **UNKNOWN**; incomplete zero counters do not establish zero usage or charge.

The serialized diagnostic does not include `valueState` for this rejection. Inspection of the accepted parser shows that `prompt_tokens`, `completion_tokens` and `total_tokens` must each have been present and parsed as unsigned integers to reach this comparison; that is a **source-derived inference**, not an independently retained raw response. Rejection occurred in `validate_completion → parse_managed_usage`, before aggregation, accepted settlement and parsing of usage details or cost. The receipt does **not** preserve separate prompt/completion values, reasoning tokens, other usage details or keys, cost presence/type/value, or the pinned subscription-proxy rule for this shape. The difference of 332 cannot safely be labeled reasoning or a charge. [Public xAI REST examples](https://docs.x.ai/developers/rest-api-reference/inference/chat-completions) of additive reasoning are context, not proof of this pinned CLI-proxy response's contract. The first response failed accounting; the later `max_total_tokens_usage_unavailable` stop is downstream unknown-usage recovery, **not** a one-request-cap denial (denied requests: zero). No semantic repair or synthetic claim about this live shape is justified.

Work remains in durable `review`, Run `interrupted`, Attempt `review`, with candidate `sha256:missing` and no source application. Three **credential-free local** reopens retained the same identities, typed rejection, one send, zero accepted usage receipts and revoked/uncertain state. The latter two reopens were stable. The fixture HEAD/tree, both source-file bytes, clean status, no remotes, external oracle hashes and empty worker root matched pre-dispatch; five prior private historical record hashes also matched. The previous failed live Work and its UNKNOWN usage/cost were not rewritten. No second provider request, fallback, rearm, approval, apply or discard occurred. [Sanitized one-request receipt](real-managed-assignment-v1-artifacts/one-request-usage-diagnostic-20260929.json).

**Result: DIAGNOSTIC INCONCLUSIVE.** This pass identifies the precise parser rejection but lacks the response breakdown and exact pinned proxy/CLI semantics needed to decide whether the arithmetic mismatch is a compatibility defect or a genuinely unsupported response. Production accounting remains fail-closed; there is no executable change or new functional CI claim. Existing exact-functional Hosted Desktop [run 36518345360](https://github.com/chriscase/GrokPtah/actions/runs/36518345360), attempt 1, succeeded on the unchanged functional SHA. The publication commit and post-functional documentation-only diff are recorded in the PR handoff. PR #580 remains draft and unmerged for review; no further live request, release or deployment follows from this observation.

## Early-rejection usage evidence and pinned-CLI projection — OFFLINE ONLY (2026-09-29)

This implementation begins at published `34690219e7533dfb90631593f809ace3f4eef2b0` (tree `a27a9736f66834e71cd2eb0f77d7545308b2397d`) on draft PR #580. Refreshed `origin/main` remained `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d`. The accepted predecessor functional revision `88a1b893715aec370e3ec6f542a18729fffe6592` (tree `500ddfb47ed859209a872c6959bded7c9cfcd44f`) remains an ancestor. New functional revision `6c91c00b49d97555a5e6e5de8eaf05fb90066e3f` (tree `8ecc48bc0a74714f9fcee7e6ea945f5c3fee0802`) changes only the bridge's managed-provider observation code and offline tests. **Usage acceptance, reconciliation, lease authority, limits, confinement, model route and source application are unchanged.** The historical `f619e7b1-6c94-4dc1-aab7-39db76db8362` response remains one HTTP 200 and `conflicting_total` 3,946 versus 3,614 with UNKNOWN accepted usage/cost; its missing component fields were not reconstructed.

### Red-to-green production boundary and bounded schema

A new local relay/canonical test supplied **synthetic** prompt 3,500, completion 114, total 3,946, cached 240, reasoning 332 and cost 1,665 ticks. On the starting code, the actual post-HTTP rejection produced `conflicting_total` but no sibling snapshot, and the test failed at its missing-snapshot assertion. The normal tool sandbox first blocked local socket binding; the native loopback rerun reached that meaningful failure. On the new functional revision, the same relay rejects the receipt exactly as before and retains separate allowlisted observations. These synthetic operands are **not** a reconstruction of the historical response. [Typed offline receipt and representative serialized snapshot](real-managed-assignment-v1-artifacts/usage-observation-offline-20260929.json).

The existing `providerEvidence` now has optional `usageObservation`. `None` means an older record has **unavailable** observation evidence, not zero. A fresh lease holds at most **two** fixed-shape receipt snapshots, each at most **3,072 serialized bytes**, with **13** fully qualified known leaf paths. Prompt audio and completion audio are distinct. The snapshot also marks each known parent as missing, null, object or wrong type; each leaf distinguishes missing, null, integer zero, positive, negative, non-integral, wrong type and unavailable parent. Known integer values remain exact through **65,536 tokens** or **1,000,000,000,000 cost ticks**; bounded non-integral numbers retain a canonical numeric value of at most **32 characters**. Out-of-bound values retain state/sign with `valueOmitted=true`, never a clamped number. Unknown keys at the top, prompt-details and completion-details levels contribute only a count up to four and a `more` bit; their names and values are suppressed. Wrong-type string/array/object content is suppressed, and a known credential echo suppresses the entire snapshot. Collection inspects only 13 known paths and at most five unknown keys per known object; the preexisting 256-KiB response ceiling bounds parsing. The existing 32-diagnostic cap now states `diagnosticsTruncated` if exceeded. Repeated receipts keep at most two observations; additional ones are explicitly marked omitted. None of this data becomes `ManagedUsage`, a settled response, a refunded reservation, restored authority or approval.

The production-boundary matrix covers accepted ordinary and supported subset/cache receipts; additive and unrelated total mismatches; missing or malformed reasoning/details; missing, null, zero, positive, negative, non-integral and wrong-type cost; unsupported known details and unknown top/nested keys; oversized numbers; repeated receipts; and credential/prose canaries. Early failures no longer hide safe sibling fields. Rejection kind, canonical uncertainty, revocation, no-retry and zero accepted usage remain unchanged. The SHA-verified installed CLI's **offline** additive-reasoning managed journey retained the new snapshot through one Work/Run/Attempt, one local fixture send, finalization and three credential-free reopens; all three retained identical provider evidence, with unchanged fixture source tree `1ef9c0872b4eadfaae5f75bbf2933c062cce67ec` and empty worker root. Its synthetic Work is `68996406-455a-425f-b8b0-0d9dd19cb0b0`; its private full report SHA-256 is `ef49af4958c8337d19918f0ea7f5b9f91984feddae413762a4aa71efba3c505e`.

### Pinned binary projection, distinct from provider semantics

The installed Grok CLI **1.0.41** (revision label `4220f3b224a6`, SHA-256 `9c844eb13365180787d9ad22b2b3748a024be8e1ed845253cc114781b31c591d`) was exercised with the production `configure_managed_provider` custom-model config and managed-child OS sandbox, disposable HOME/GROK_HOME/workspace, a synthetic local capability, and a separate loopback-only upstream. The production validator was **not** disabled. Each fixture caused one intended `grok-build-0.1` request and one auxiliary local `grok-4.6` request; the CLI journal counted only the intended model call. No external request was permitted by the sandbox. The [sanitized four-fixture projection report](real-managed-assignment-v1-artifacts/pinned-cli-usage-projection-20260929.json) records exact synthetic input, bounded headless fields, durable session/turn fields and safe request shapes, without prompts or credentials.

| Provider field in the synthetic additive case | Pinned CLI headless projection | Durable CLI session/turn | Production host treatment |
| --- | --- | --- | --- |
| `prompt_tokens=100`, `cached_tokens=20` | `input_tokens=80`, `cache_read_input_tokens=20` | `inputTokens=100`, `cachedReadTokens=20` | Full prompt 100; cache is a subset, never added twice. |
| `completion_tokens=10`, `reasoning_tokens=5` | `output_tokens=10`, `reasoning_tokens=5` | `outputTokens=10`, `reasoningTokens=5` | The accepted host contract requires reasoning to be a completion subset; this synthetic additive form is rejected. |
| Provider `total_tokens=115` | `total_tokens=110` | `totalTokens=110` | Reject `conflicting_total` before success; retain provider 115 only in diagnostic observation, not settled usage. The CLI recomputed 100 + 10. |
| `cost_in_usd_ticks=777` | `total_cost_usd_ticks=777`, `total_cost_usd=0.0000000777`, model `costUSD` likewise | Session/turn `usage.json` has no cost field | Diagnostic retains exact ticks; host does **not** accept the additive receipt or infer a bill. |

The ordinary missing-cost fixture reported 100/10/110, and both CLI projections reported 110 with no cost field. The supported reasoning-subset/cache fixture also reported total 110: headless split prompt into 80 uncached plus 20 cached, retained reasoning 3 and 777 ticks; its journal kept full input 100 and total 110. For a **reported zero** cost, the CLI omitted cost fields just as it did for missing cost, so those two upstream states are **not distinguishable from CLI headless or journal output**. The host's new snapshot distinguishes them, while existing acceptance continues to treat missing as unknown and reject reported zero. The additive fixture demonstrates a client-side transformation of synthetic provider total 115 to CLI total 110; it is not permission to replace a provider-reported total or label the live difference of 332 as reasoning.

[Public xAI Chat Completions documentation](https://docs.x.ai/developers/rest-api-reference/inference/chat-completions) includes an additive example with prompt 32, completion 9, reasoning 94 and total 135. [Public cost documentation](https://docs.x.ai/developers/cost-tracking) defines 10 billion ticks per USD and final-stream usage/cost; [prompt-cache documentation](https://docs.x.ai/developers/advanced-api-usage/prompt-caching/usage-and-pricing) describes cached prompt usage. Separately, the first-party public source at `xai-org/grok-build` commit `9684fa3cdbf2995e30ea8b9b637f1db008f144fc` ([headless guide](https://github.com/xai-org/grok-build/blob/9684fa3cdbf2995e30ea8b9b637f1db008f144fc/crates/codegen/xai-grok-pager/docs/user-guide/14-headless-mode.md), blob `cadeec22e94add52acb626f29df194ed97f22f41`; [session usage source](https://github.com/xai-org/grok-build/blob/9684fa3cdbf2995e30ea8b9b637f1db008f144fc/crates/codegen/xai-grok-shell/src/session/signals.rs), blob `89165e88eb2647cdfb5f91ee17675d87fff8e4d2`) describes client-side usage. That public revision is **not established as the source of the pinned binary**, and the public REST API is **not established as the subscription proxy's accounting contract**. The exact pinned binary experiment establishes only the client projections above.

**Accounting decision: no acceptance change.** A possible future narrow adapter would retain provider-reported and CLI-derived totals as separately named values, reconcile their documented relationship, and reserve every supported token component before send; it cannot simply accept `total >= prompt + completion` or discard extra reasoning, cache or charge. Its prerequisite is an authoritative pinned subscription-proxy contract for whether reasoning is included in completion and total, how `max_tokens` bounds each component, and cost/detail semantics, together with a complete observed usage shape under a separately authorized qualification. The historical live response lacks those details. The original live G1–G10 goal remains incomplete; this pass makes no new live request or historical Work mutation.

Final offline validation: serial bridge **745 passed, 0 failed, 10 ignored** with disposable host state and external HTTP(S) routed to a closed local proxy; eight installed-CLI managed journeys, the four-form direct pinned-binary projection test and the separate production protocol/no-retry installed-CLI test all passed against local upstreams. Formatting, strict all-target Clippy and diff checks passed. The early red native-local test failure was expected; the initial ordinary sandbox bind failure did not reach its assertion. Five prior private historical records remain byte-identical to the pre-dispatch manifest; the latest live execute/review records predate this goal and still show one wire, zero completed usage and uncertainty. Additional live provider requests, credential/account maintenance, historical approve/apply/discard, merge, release and deployment: **ZERO**.

**Exact-functional Hosted Desktop:** [run 36593888063](https://github.com/chriscase/GrokPtah/actions/runs/36593888063), attempt 1, `pull_request`, head `6c91c00b49d97555a5e6e5de8eaf05fb90066e3f`, **completed/success**, one successful job and zero failed steps. The later evidence-only publication identity is recorded in the PR handoff; it does not change this tested executable revision. PR #580 remains draft and unmerged for independent review.

## Complete one-request usage observation — specific pre-send bound follow-up (2026-09-30)

**Status: OBSERVED CONTRACT INCOMPATIBLE — SPECIFIC FOLLOW-UP REQUIRED.** The single authorized request captured the complete bounded usage breakdown. This response explicitly reports additive reasoning; the pinned CLI's established projection can be described precisely. A safe acceptance adapter is not justified because no applicable pre-send bound on completion **plus** reasoning has been established for this subscription-proxy route. Production accounting remains unchanged. This bounded observation assignment stops for review; the original live G1–G10 qualification remains incomplete.

### Executed identity and admission

The accepted executable source remains functional `6c91c00b49d97555a5e6e5de8eaf05fb90066e3f`, tree `8ecc48bc0a74714f9fcee7e6ea945f5c3fee0802`. Starting published tip `1883889033c63f7a52a016d6ccff5644519b238e`, tree `bfae8d20094af83dab2585dc6aad4b7c84234b69`, differs only in the three prior evidence paths. Refreshed main remained `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d`, tree `f524b705a3b88cb895309506bf726e76c91874bf`. The existing one-request wrapper was rebuilt offline against that unchanged bridge source: wrapper source SHA-256 `2482cc0d28f35223a65a34196d8cbc519e275b527ba06fe431d14cdb955efbfb`, executed binary SHA-256 `7be1beba034d6be5c32052e1148a126ecfcb867c7cc896b62dd1eff5aa053d38`. Grok CLI **1.0.41**, revision label `4220f3b224a6`, retained SHA-256 `9c844eb13365180787d9ad22b2b3748a024be8e1ed845253cc114781b31c591d`.

Two earlier launch requests were rejected by automatic approval review before process creation or dispatch. The owner then directly approved the unchanged request-scope manifest, SHA-256 `e247612ef5189cf04237ec84e5f026989bcc286aac9e9d0e9d69d2e269c6cf2f`, including runtime CLI-generated messages and tool schemas confined to the synthetic two-file task. Production prepare/start/status and canonical admission were used. Immediately before launch, local cached OIDC attestation established at least ten minutes of freshness; no refresh, login, entitlement or other provider/account call occurred. Readiness was READY with **zero** worker dispatches and provider invocations. That pre-dispatch zero remains distinct from the subsequent authorized one send.

The remote-free disposable source retained HEAD `f3fe486b7f455f09aa61269325aabc21afbf0e26` and tree `1ef9c0872b4eadfaae5f75bbf2933c062cce67ec`, with only `src/framing.py` and `src/decoder.py`. The host-owned external oracle baseline failed with exit 1. Bounds were sealed before capability issuance: **one** worker attempt and physical request admission, **180,000 ms**, **16,000** aggregate-token ceiling, **1,024** wire output limit, **16,000** prompt bytes, **65,536** request bytes and **262,144** response bytes. Existing tool/isolation limits, zero retries and zero application remained. The child received only its revocable local relay capability; the host held the official Grok Build OIDC authentication. Destination, model and accepted model-override header remained `POST https://cli-chat-proxy.grok.com/v1/chat/completions`, `grok-build-0.1`, and the pinned host-controlled route.

### Complete observation and canonical failure

The sole start created Work `bc84206c-fa43-492a-9ef3-6b0c4f0d812f`, Run `28b09b4f-3311-4372-ade8-6712bb34e0f9` and Attempt `9e69b1b7-20ec-4ea8-a662-40270dafc6e6`. There was **one physical upstream request**, one HTTP **200**, safe provider request ID `81131b44-625b-9118-9bd9-872fa6eec761`, and zero accepted usage settlements. The bounded response drained before validation. `validate_completion → parse_managed_usage` rejected `total_tokens`: `conflicting_total`, observed **3,862**, expected prompt plus completion **3,602**. The canonical audit contains one intent, one wire admission and one `uncertain` / `ProtocolAfterPossibleEffect` outcome, authority attempt `att_fecce8f8f85b7544`. No second CLI turn reached admission; denied requests are **zero**. This first-response accounting rejection is not a stop caused by the one-request cap.

The retained snapshot has all **13** known leaf observations, object states for all three known parents, unknown-key counts of zero, and no credential/content suppression, omitted values or truncated diagnostics. Its compact serialization is **2,020 bytes**, below the accepted 3,072-byte ceiling. [Complete sanitized receipt, exact states, provenance and preservation](real-managed-assignment-v1-artifacts/complete-usage-observation-20260930.json) retains the full typed observation without raw response body, prompt, tool arguments, arbitrary provider keys, credentials or private transcript.

| Observed provider value | Host accounting on the executed revision | Expected pinned CLI headless projection | Expected durable CLI journal projection |
| --- | --- | --- | --- |
| Prompt **3,591**, text **3,591**, cached prompt **128** | Diagnostic observation only; no accepted settlement | Uncached input **3,463**, cached input **128**, cache creation **0** | Full input **3,591**, cached read **128**, cache creation **0** |
| Completion **11**, explicit reasoning **260** | Additive form rejected; reasoning is not inferred from a difference | Output **11**, reasoning **260** | Output **11**, reasoning **260** |
| Provider total **3,862** = 3,591 + 11 + explicit 260 | Settled total unavailable; conditional additive budget count would be **3,862**, counting cache only within prompt | Total **3,602** = 3,463 + 128 + 11 | Total **3,602** = 3,591 + 11 |
| Reported positive cost **40,306,000 ticks** | Retained as a provider observation; accepted cost settlement unavailable | Expected ticks **40,306,000** if forwarded | Cost unavailable in established session/turn usage schema |

All other observed known audio/image/prediction counts and `num_sources_used` are explicitly **zero**. The headless and journal columns are **derived expectations from the accepted prior synthetic pinned-binary campaign**, not live measurements: this rejected receipt was never forwarded to the CLI. The CLI projection campaign was not repeated. Public tick conversion gives a reported USD equivalent of **0.0040306**; this is **not** proof of final subscription billing. Final account billing and accepted settlement remain **UNKNOWN**. Missing, reported zero and positive cost remain distinct diagnostic states; existing acceptance was not changed. The historical **332**-token difference remains unclassified and its discarded components are not reconstructed.

### Precise contract fact required before an adapter

The [official request schema](https://docs.x.ai/developers/rest-api-reference/inference/chat-completions) documents `max_completion_tokens` as a visible-output limit excluding reasoning and function calls; it deprecates `max_tokens` without specifying a joint bound. Its additive example matches this observed shape, although the `total_tokens` prose still describes prompt plus completion. The public Markdown schema was fetched without credentials on 2026-09-30 and pinned by SHA-256 `ddb3f09fa7eb5977cf623643562bc4f862b7bc71ffe47ea8f4d999728e7d2d60`. [Official cost documentation](https://docs.x.ai/developers/cost-tracking) supports the tick conversion. [Official enterprise route documentation](https://docs.x.ai/build/enterprise) identifies the CLI proxy separately from the API-key route. Public API semantics are not automatically every subscription-proxy guarantee.

The accepted production relay sets `max_tokens=1024`, removes `max_completion_tokens` and `reasoning_effort`, and retains its existing `x-grok-effort: low` header. Before send it reserves checked prior accounted total plus incoming request bytes plus 1,024. There is no separate enforceable reasoning allowance in that path. One response with only 271 combined completion/reasoning tokens cannot establish a worst-case cap. An additive adapter could correctly preserve provider 3,862 and compare CLI 3,602, but a post-response check would not prevent already-incurred reasoning usage beyond the reservation.

**Specific follow-up:** establish an applicable provider contract that `max_tokens=1024` jointly bounds chargeable completion plus reasoning for `grok-build-0.1` on the pinned OIDC CLI-proxy route, or a supported independently enforceable pre-send reasoning/total cap within the existing ceilings. This is a concrete output-budget question; no unavailable server implementation commit is required. No second live experiment, fallback, larger limit, response rewrite or alternate ledger is authorized. Until that fact is established, no executable acceptance repair is justified. The complete additive observation and precise expected client mapping are now available for review.

### Retirement, preservation and validation

Capability retirement preceded offline reconciliation. Work remains `review`, Run `interrupted`, Attempt `review`, candidate `sha256:missing`, and stop reason `managed_execution_uncertain_or_failed`; source application and approval are false. **Three credential-free reopens**, under an OS sandbox denying external network and permitting loopback only, retained identical Work/Run/Attempt and provider evidence. The external-denial probe returned `EPERM`. Source HEAD/tree/status/remotes and both file hashes, external oracle hashes, empty worker root and **eleven** prior private record hashes all match their pre-dispatch identities. All **60** prior tracked artifact files remain byte-identical, and the preceding evidence text is preserved as a byte-identical prefix. Historical failed attempts and unknown usage/cost qualifications remain unchanged.

Offline receipt validation checked exact allowlisted keys, parent/leaf states, numeric bounds, explicit arithmetic, canonical one-send events, all three reopen identities and all pinned binary/source/oracle/historical hashes. It passed in the external-network-denied sandbox. No executable change was justified, so no new product build or projection campaign was run, and no new retained-candidate/oracle journey is claimed for this unsupported form. The earlier accepted offline production work remains intact. Refreshed exact-functional Hosted Desktop [run 36593888063](https://github.com/chriscase/GrokPtah/actions/runs/36593888063), attempt **1**, still reports **success** on unchanged functional `6c91c00b49d97555a5e6e5de8eaf05fb90066e3f`, one successful job and zero failed steps. The new publication is evidence only; final published SHA/tree are recorded in the PR handoff.

Additional live requests after the sole diagnostic, credential/account maintenance, retries/rearms/fallbacks, source approval/application/discard, merge, release and deployment: **ZERO**. PR #580 remains draft and unmerged. This pass ends with the precise contract follow-up above and does not authorize another request or another goal.


## Responses output-bound qualification — OFFLINE ONLY (2026-09-30)

**RESPONSES MANAGED ROUTE READY FOR INDEPENDENT REVIEW.** Starting published SHA `81cddffd9e36ac2da3b61dfb2706471bc4a11aeb`, tree `a3affa014cb4407e0a5c6aa6f51389c264b8e8a0`; prior accepted executable `6c91c00b49d97555a5e6e5de8eaf05fb90066e3f`, tree `8ecc48bc0a74714f9fcee7e6ea945f5c3fee0802`. Refreshed main remains `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d`, tree `f524b705a3b88cb895309506bf726e76c91874bf`. New functional SHA **`b21b7f4d1e3d139e4b40ca8d9f6b38e1c5622c73`**, tree **`e7fef974f53e9ff5f1aaf15375431bf8fecac8ff`**. The supplied goal is frozen byte-for-byte in [the goal document](real-managed-assignment-v1-responses-offline-goal.md). Final evidence SHA/tree are recorded in the PR handoff after the documentation-only commit.

### Contract and bounded implementation

The [primary xAI Responses contract](https://docs.x.ai/developers/rest-api-reference/inference/responses) explicitly includes reasoning in `max_output_tokens`. This establishes the normative pre-send generated-token bound. [Contract evidence](real-managed-assignment-v1-artifacts/responses-offline-20260930-contract.md) records the exact short quote, research date, primary route/tool sources, inconsistent example qualification, and the inference connecting this API dialect to the documented OIDC inference proxy. There is no separate proxy-specific cap statement or live measurement of server enforcement. Current first-party source corroborates routing only; it is not proof of pinned CLI behavior. **Live Responses compatibility and full live-provider/frozen-goal completion are not claimed.**

Only six bridge Rust paths change: managed-provider relay/evidence, new narrow Responses codec, existing offline integration fixture, CLI configuration/reconciliation, canonical wire admission, and the service's managed profile selection. [Functional identities and exact executable path/hash list](real-managed-assignment-v1-artifacts/responses-offline-20260930-functional-identity.json). No generic provider rewrite, credential-store change, limit increase or legacy additive accounting adapter is introduced.

The managed profile selects host-held OIDC at the official CLI proxy, `grok-build-0.1`, Responses and `max_output_tokens=1024`; the host seals client file tools, reasoning configuration, route and byte limits. Backend/model/cap substitution and provider tools fail closed. Before every physical send, checked settled total + conservative complete text request bytes + owned opaque-reasoning reuse allowance + 1024 must fit **16000**. Reused reasoning is bound to this lease's settled fingerprint; its allowance comes from all prior generated output, not ciphertext length. Cache never creates authority.

Responses mapping is `total=input+output`, with reasoning and cache retained as subsets. Synthetic 100/15/5/115 charges **115**, never 120. Cost absence remains UNKNOWN; explicit zero remains known zero. Contradictory or unknown usage is terminal and cannot restore request authority. A valid incomplete/max-output receipt is bounded and durably settled, but revokes capability and cannot yield a successful harvest. Complete bounded streams and function-call coherence are checked before any response bytes reach the child. Existing completion/abandonment guards are shared across both dialects. No remote Responses idempotency promise is assumed.

### Exact pinned binary and actual production journey

The installed CLI is **1.0.41**, revision `4220f3b224a6`, SHA-256 **`9c844eb13365180787d9ad22b2b3748a024be8e1ed845253cc114781b31c591d`**. Each qualification verifies this digest before launch under the existing deny-default native macOS sandbox, disposable HOME/GROK_HOME, synthetic OIDC and loopback-only access. The child has only local revocable capability, no real credential or operator files. All intended sends are `POST /v1/responses`, model `grok-build-0.1`, cap 1024, summary `concise`, with sealed OIDC/model-routing headers and seven local function tools including the CLI's file-tool discovery wrappers. The exact binary attempts a local title-model request despite `title_refresh=false`; the production relay denies it before admission, with **zero auxiliary upstream authority**. No update, telemetry, plugin, MCP, web or background upstream traffic was authorized.

[Pinned A–H headless and durable journal captures](real-managed-assignment-v1-artifacts/responses-offline-20260930-pinned-projections.json) show distinct cached/full input projections, two model rounds within one journal user turn, nonzero reasoning, known-zero cost omission, incomplete handling, and client behavior on contradictory/unknown forms. The host refuses F/G/H before forwarding; permissive client projection is never acceptance authority.

[Ten integrated production cases](real-managed-assignment-v1-artifacts/responses-offline-20260930-journeys.json) traverse prepare → Work → Run → Attempt → relay → canonical authority → local Responses → actual file tools → retained candidate → host oracle → review. Each begins with zero readiness dispatch/invocation counts and a red source oracle. A/B/C/D produce meaningful two-file green candidates; B/C include nonzero reasoning, D completes with final output exactly 1024. E settles one bounded incomplete receipt without successful harvest; F/G/H/missing/malformed stop after one fixture send. Every case has exactly one Work/Run/Attempt, an unchanged source, no Responses approval/application, and three reopens with identical provider evidence and zero additional sends. Case C's second reservation is **115+14645+15+1024=15799**. [Boundary table and adversarial coverage](real-managed-assignment-v1-artifacts/responses-offline-20260930-boundaries.md) distinguish direct binary projections from integrated journeys. [Meaningful pre-canonical-admission red](real-managed-assignment-v1-artifacts/responses-offline-20260930-red.json) reserved one request but made zero wire sends; preliminary sandbox/fixture setup failures are separately qualified.

### Validation and preservation

All required local checks passed on these unchanged functional source bytes:

| Check | Result |
| --- | --- |
| Focused managed relay/accounting | 52 passed, 0 failed; 10 CLI tests ignored in this focused invocation |
| Explicit pinned-CLI offline suites | 11 test functions passed; includes ten Responses and eight legacy integrated modes, plus legacy projection/protocol campaigns |
| Full offline locked bridge, fresh GROKPTAH_HOME | 1284 passed, 0 failed, 15 ignored across 39 targets; includes 102 Verified Change workflow tests |
| Bridge format / strict all-target Clippy | Passed |
| Service check / format / strict all-target Clippy / tests | Passed; 20 tests |
| Host authority | 119 tests including doctests passed |
| Frontend typecheck / tests | Passed; 428 tests in 58 files |
| Tauri | 50 tests passed |

During concurrent validation an existing service disconnect/reconnect test saturated its journal queue and retained an unclean-shutdown lock. Its isolated rerun passed, then the quiet full service suite and all checks passed. This is retained as a qualification, not hidden or repaired through an unrelated durability change. Counts exclude nested child-process test results.

**Exact-functional Hosted Desktop [run 36782699253](https://github.com/chriscase/GrokPtah/actions/runs/36782699253), attempt 1, completed successfully on `b21b7f4d1e3d139e4b40ca8d9f6b38e1c5622c73`.** All jobs succeeded, with zero failed steps. [Validation commands, target counts, log digests and hosted result](real-managed-assignment-v1-artifacts/responses-offline-20260930-validation.json). The later evidence commit changes documentation only and retains those exact executable bytes.

[Preservation proof](real-managed-assignment-v1-artifacts/responses-offline-20260930-preservation.json) checks 15 private historical records, four source/oracle files and all 61 prior published artifacts unchanged. Historical Work `bc84206c-fa43-492a-9ef3-6b0c4f0d812f`, Run `28b09b4f-3311-4372-ade8-6712bb34e0f9`, Attempt `9e69b1b7-20ec-4ea8-a662-40270dafc6e6` remains rejected/uncertain with observed additive 3591+11+260=3862, cached 128, cost 40306000 ticks, and final billing/accepted settlement UNKNOWN. It was not reinterpreted, retried, rearmed or otherwise mutated. Legacy Chat additive reasoning still fails closed. The preflight's original zero-dispatch fact and preceding evidence text remain preserved.

**Additional live provider requests: 0.** No login, refresh, account/entitlement/model-list/billing call, new Responses source application, merge, release, deployment or expanded goal occurred. PR #580 remains draft and unmerged. Independent review should assess the contract applicability inference, sealed pre-send reserve including opaque reasoning, strict stream/accounting and terminal ownership, pinned projections, real offline journey and exact-functional CI. Stop here for that review; no further dispatch is authorized by this result.
