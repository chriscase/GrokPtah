# grokptah-agent-sdk

The versioned, provider-neutral capability boundary for embedding the GrokPtah
agentic harness in another project.

This crate is **contract only**: traits, DTOs, an error taxonomy, a capability
document, and a deterministic fake. It depends on nothing from
`grokptah-agent-bridge`, opens no sockets, and touches no filesystem.

```text
  ContextDesk / another UI
           │  depends on
           ▼
  grokptah-agent-sdk   ← traits + DTOs + errors + capability document
           ▲  implemented by
           │
  ┌────────┴─────────┬───────────────────┬──────────────────┐
  │ desktop adapter  │ service adapter   │ FakeControlPlane │
  │ (in-process)     │ (MCP over HTTP)   │ (deterministic)  │
  └──────────────────┴───────────────────┴──────────────────┘
           │ calls
           ▼
  grokptah-agent-bridge (runtime; never a consumer dependency)
```

## Use it

```toml
[dependencies]
grokptah-agent-sdk = { path = "../GrokPtah/crates/codegen/grokptah-agent-sdk" }
```

```rust
use grokptah_agent_sdk::prelude::*;

async fn run(plane: &dyn AgentControlPlane) -> SdkResult<()> {
    let connected = plane.connect().await?;      // discovery + version negotiation
    connected.require(&CapabilityId::TaskSubmit)?;
    // ... submit_task / observe_run / stream_events / cancel_run
    Ok(())
}
```

Build your UI against `FakeControlPlane` first — it produces every failure mode
the boundary defines, deterministically and with no host running.

## Verify

```sh
cd crates/codegen/grokptah-agent-sdk
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

## Status

Pre-1.0 and `publish = false`. ADR-002 §7 gates SDK publication on a named
compatibility owner maintaining the parity matrix for a real external consumer.
The matrix is `conformance::run_battery`; the design, adapter mapping, and
residual work are in [`docs/AGENT_SDK_SEAM.md`](../../../docs/AGENT_SDK_SEAM.md).
