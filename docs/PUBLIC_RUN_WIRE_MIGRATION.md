# Public-run wire migration (staged history)

> **Current status at consolidation `40ecba6107504299f797d95b196c2408986d1976`:**
> The scoped MCP list/get/progress/handoff producers emit
> `grokptah.public-run.v1`; the SDK, Tauri remote path, Node probes, bridge
> tests, and certification probes consume the allowlisted document. Legacy raw
> readers fail closed without invoking a public tool. The remainder of this
> file is historical staging context and must not be read as the current wire
> contract. Public event history is separately versioned as
> `grokptah.public-event.v1`.

Historical status (before the local consolidation snapshot): **not switched**.
Commit `4bbf0c499aa65a7ccfa4fa1f5578c65360ab501d`
added `orchestration::public_run::{PublicRunV1, PublicRunListV1,
PublicRunProgressV1, PublicRunHandoffV1}` with fail-closed parse
(`schemaVersion = grokptah.public-run.v1`, `deny_unknown_fields`). MCP
`ptah_list_runs` / `ptah_get_run` / `ptah_get_progress` / `ptah_get_handoff`
still serialize raw `RunRecord` or ad-hoc JSON that includes prompt, path,
workspace, and other private fields.

Do **not** adopt the DTO on any one of those four tools until every consumer
below parses only the allowlisted v1 document in the same change. A partial
switch leaves one public path on `RunRecord`.

The DTO omits fields current public contracts still treat as required:
`sessionId`, `workspace`, `clientId`, `retryOf`, `parentRunId`, `agentId`,
`agentSpecRevision`, `checkpointId`, continuation hashes, `bounds`,
`stopCause`, nested `aggregates` / `progress` / `verification`,
`finalResponse`, `promptPreview`, `startSeq` (renamed `eventStartSeq`).
Switching without restaging those proofs would drop coordinator lineage,
token stop-cause, device attribution, and desktop remote inspect.

## Remaining consumers

Exact producer symbols (still `RunRecord` / ad-hoc JSON):

| Path | Symbol |
| --- | --- |
| list | `OrchestrationService::list_runs_scoped` → `json!({ "runs": Vec<RunRecord> })` |
| get | `run_value` via `get_run` / `get_run_scoped` |
| progress | `progress_value` via `get_progress` / `get_progress_scoped` (`promptPreview`, nested `progress`) |
| handoff | `handoff_for_run` via `get_handoff` / `get_handoff_scoped` (`finalResponse`, `changes`, `tests`, `verification`) |
| MCP dispatch | `mcp_control::dispatch_tool` arms for the four tool names (thin adapter; no extra serialize) |

Parsers / UI still decoding that wire:

1. **SDK** `grokptah-agent-sdk::dto::{project_run, project_runs, RunView}` — strips secrets client-side from camelCase `RunRecord`; keeps `session_id`, `workspace`, `bounds`, `stop_cause`. Fail-closed `grokptah.public-run.v1` parser is staged as `parse_public_run_v1` / `parse_public_run_list_v1` / `parse_public_run_progress_v1` / `parse_public_run_handoff_v1` (same allowlisted keys as the bridge DTO, `deny_unknown_fields`, `SdkError::Internal` on unknown version/field). It is not wired to `ReadObservatory`. Crate must not depend on `grokptah-agent-bridge`. Session/workspace are not read from the document; stamp them from the list/get request when wiring.
2. **Desktop remote MCP** `desktop/src-tauri/src/remote_service.rs` `list_runs` / `get_run` (`serde_json::from_value::<Vec<RunRecord>>` / `RunRecord`).
3. **Desktop Tauri** `commands.rs` `remote_service_run_list` / `remote_service_run_get` return `RunRecord`.
4. **Desktop protocol** `desktop/src/lib/protocol.ts` `DurableRun` + `api.ts` `remoteServiceRunList` / `remoteServiceRunGet` (same type as local host `run_list` / `run_get`, which stay internal). Fail-closed `grokptah.public-run.v1` parser is staged at `desktop/src/lib/publicRun.ts` (`parsePublicRunV1` / `parsePublicRunListV1` / `parsePublicRunProgressV1` / `parsePublicRunHandoffV1`) and re-exported from `protocol.ts`. It is not wired to `api.ts` or the inspector.
5. **Desktop UI** `App.tsx` public-run list/get refresh is poll-only (`sessionId`/`workspace` stamped from the request). It must not start legacy `remoteServiceWatchRuns` / `run_watcher`, which emits raw `SessionUpdate` journal bodies on `remote://run-event`. `runOrigin.ts` (`clientId`); `RunInspector.tsx` (`promptPreview`, `aggregates`, `bounds`, `stopCause`, `finalResponse`, `progress.lastTool` / `detail`).
6. **Service smoke/conformance** `grokptah-service/tests/service_smoke.rs` (`startSeq`); `service_conformance.rs` (`clientId == "laptop"` named-credential attribution; cancelled-history `runId`/`state`).
7. **Bridge MCP/orchestration tests** `orchestration_control.rs` (`retryOf`, `agentId`, `bounds.maxTotalTokens`, `stopCause`, `parentRunId`/`checkpointId` on list, `progress.round`/`lastTool`, `handoff.finalResponse`); `orchestration_adversarial.rs` (nested `progress`, `handoff.changes`/`verification`); `mcp_streamable_transport.rs` (`clientId`, `retryOf`, handoff `verification`/`stopCause`/`bounds`/`usage`); `mcp_live_events.rs` (`startSeq`); `mcp_soak_hardening.rs` (get/handoff state).
8. **HTTP coordinator scripts** `tests/mcp_sdk_interop/run_continuity_probe.mjs` (`durableReads` of get+handoff into derivation prompts); `run_coordinator_campaign.mjs`; `run_soak.mjs` (`handoff.finalResponse`, `changes[]`); `run_live_smoke.mjs`.
9. **Certification lab** `evals/certification-lab/src/probes.rs` — `wait_for_terminal_evidence` plus required `agentId` / `agentSpecRevision`; `aggregates.usageComplete`; list filter on `parentRunId` / `checkpointId` / continuation hashes (`core-continuation-resume-v1`).
10. **Docs that describe the current public document** `docs/MCP_CONTROL_COORDINATOR.md` (bounds, nested usage, `stopCause`, handoff `finalResponse` + `verification`); `docs/AGENT_SDK_READ_SEAM.md` (RunRecord wire); `docs/DURABLE_RUNS.md`; `docs/HEADLESS_SERVICE.md`; `docs/CONTINUITY_PROBE.md`.

Internal `RunRecord` (do not change for this migration): `OrchStore`, `AgentHostHandle` session run APIs, local Tauri `run_list` / `run_get`, computer-use `ComputerRun`.

## Staged order

1. **Product lock on v1 allowlist.** Decide whether `stopCause`, `bounds`, `clientId`, `retryOf`, and continuation lineage stay private (current DTO) or must be added to v1 before any wire change. Do not switch until this matches coordinator/cert/desktop proofs.
2. **Restage cert-lab and continuity proofs off get/list private fields** onto `ptah_get_persistent_agent`, work attempts, and `ptah_get_changes` / `ptah_get_test_results`, while the wire still has both shapes. Continuation child correlation must not require `parentRunId` on `ptah_list_runs`.
3. **Split desktop local vs remote types.** Keep `DurableRun` for in-process host. Remote parser uses `parse_public_run_*` (bridge) / staged `desktop/src/lib/publicRun.ts` (same allowlisted keys, deny unknown fields/version). Stamp `sessionId`/`workspace` from the list/get *request*, never from the run document. Public-run refresh must not register the raw watcher (`startSeq` → `eventStartSeq` is display-only until a public event schema exists). `RunInspector` remote mode must not read `promptPreview` / path aggregates / `clientId`. The TS parser is staged only; remote `api.ts` still types MCP reads as `DurableRun`.
4. **SDK parser** accepts only `grokptah.public-run.v1` + allowlisted keys; unknown version/field → existing redacted `SdkError::Internal`. Parser types are staged (`PublicRunV1` and siblings) and not mapped into `RunView`. Remaining: wire `ReadObservatory` from DTO + request `SessionScope`/`RunSelector`. Drop or optionalize `bounds`/`stop_cause` to match step 1. Bump `CONTRACT_VERSION` if `RunView` shape changes. No duplicate `from_run` conversion.
5. **One producer change** of all four: `list_runs_scoped` → `PublicRunListV1::from_runs`; `run_value` → `PublicRunV1::from_run`; `progress_value` → `PublicRunProgressV1::from_run`; `handoff_for_run` → `PublicRunHandoffV1::from_run`. MCP dispatch unchanged. Tests that needed private fields read `store().load_run()`. Assert every public body lacks `promptPreview`, `finalResponse`, `workspace`, `sessionId`, paths, `aggregates`, nested `progress`.
6. **Rewrite documented public shape** in the docs listed above in the same change. Do not publish, push, or merge until steps 2–5 are green on focused bridge/MCP, SDK, desktop protocol, format/lint, and public-consumer smoke (no provider/live/CU).

Private diagnostics, `ptah_get_changes`, `ptah_get_test_results`, `ptah_get_events`, submit receipts, and local desktop host records stay on internal types.
