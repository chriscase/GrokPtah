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
- lease- and revision-fenced Computer Use control requests.

Adapters remain responsible for mapping these contracts to MCP, applying host
policy, authenticating users, redacting data, and retaining credentials. The
desktop bridge remains the authority anchor; this crate does not authorize a
remote originator or bypass human gates.
