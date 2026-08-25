# grokptah-agent-sdk

Host-neutral Rust contracts for products that consume GrokPtah capabilities.

This crate is intentionally limited to serializable DTOs and stable validation
vocabulary. It does not execute agents or own any authority. It has no Tauri,
provider, filesystem, network, credential, or OS dependency, so a product such
as ContextDesk can depend on it from a desktop adapter or server-side broker.

The crate covers:

- versioned capability discovery;
- exact session/workspace/run identity fences;
- bounded submit requests and durable run projections;
- cursor-paged events and explicit recovery notifications;
- isolated-run review receipts and stable error categories;
- lease- and revision-fenced Computer Use control requests;
- provider-neutral external-worker launch, lifecycle, event, and artifact
  projections for cloud coding agents, including explicitly idempotent
  follow-up requests;
- identity-only external-worker list query, summary, and page DTOs plus the
  bounded list-limit constant, re-exported from the crate root for public
  consumers (`ExternalWorkerListQuery`, `ExternalWorkerSummary`,
  `ExternalWorkerListPage`, `MAX_EXTERNAL_WORKER_LIST_LIMIT`).

The boundary DTOs that accept caller-controlled data expose a `validate()`
method. Consumers should call those validators before crossing a process or
product boundary; the authority still applies its negotiated workspace,
capability, and host ceilings after validation. Read-only page/projection
wrappers remain data-only and inherit the validation of the records they carry.
Invalid bounds, empty identity fences, absolute review paths, oversized
event/detail payloads, and zero-duration Computer Use leases fail closed.

Adapters remain responsible for mapping these contracts to MCP, applying host
policy, authenticating users, redacting data, and retaining credentials. The
desktop bridge remains the authority anchor; this crate does not authorize a
remote originator or bypass human gates.

The in-tree `McpControlClient` is a transport and negotiated-capability helper;
its `call_tool` method does not itself satisfy a descriptor's `human_gate`.
Authority-aware adapters should use the desktop operation facade (or an
equivalent approval/lease layer) before invoking promotion or Computer Use
control, and should never treat transport reachability as approval.

A second-product consumer such as ContextDesk must import launch, list, archive,
and error types from this crate root. `tests/context_desk_consumer.rs` is the
disposable crate-root fixture for that import surface. It is not a live
ContextDesk HTTP integration and does not grant native desktop authority.
