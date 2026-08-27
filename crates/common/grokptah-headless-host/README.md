# grokptah-headless-host

Start, observe, steer, pause, resume, and durably recover GrokPtah runs without
the desktop app.

This crate is a library plus one binary (`grokptah-headless`). It owns process
lifecycle, durable state, authority enforcement, and truthful projection. It
does not own model execution, credentials, or transport — those arrive through
injected ports or not at all.

Design, authority tier, and residuals: [`docs/HEADLESS_HOST.md`](../../../docs/HEADLESS_HOST.md).

## Shape

```text
config ─▶ home lock ─▶ store (recover) ─▶ authority ─▶ engine port
                                              │
                        redaction ─▶ journal ─┴─▶ projections ─▶ NDJSON control
```

- **Fail closed.** An unconfigured capability is denied, a gated capability
  without an explicit grant is denied, a request above a ceiling is refused
  rather than clamped, and an unresolved escalation expires to *deny*.
- **Exclusive home.** One host owns one home. The desktop home is refused by
  configuration, and a second host is refused by the lock.
- **No credentials.** The dependency tree is `serde`, `serde_json`, and the
  contract SDK; `clap` and `tokio` are optional and carry only the binary and
  its signal wiring. No keyring, no D-Bus, no browser, no HTTP client.
- **Deterministic.** Time, identity, and execution are injected, so the whole
  lifecycle is exercisable offline with no provider credential and no network.

## Configuration

```json
{
  "home": "/var/lib/grokptah-headless",
  "workspace": "/srv/projects/example",
  "sessionId": "headless-1",
  "capabilities": {
    "contract": "grokptah.capabilities.v1",
    "capabilities": [
      {
        "id": "session.observe",
        "tier": "observe",
        "mutating": false,
        "human_gate": false,
        "availability": "available",
        "description": "Read bounded, redacted host and run projections."
      },
      {
        "id": "run.execute",
        "tier": "execute",
        "mutating": true,
        "human_gate": false,
        "availability": "available",
        "description": "Submit, cancel, and pause bounded runs."
      }
    ]
  },
  "grants": [],
  "limits": { "maxActiveRuns": 1, "maxRounds": 8, "eventRetention": 256 },
  "engine": { "kind": "fixture", "script": "/srv/projects/example/fixture.json" }
}
```

`limits` and `grants` may be omitted; every limit has a bounded default and no
grant is the default-deny position. Unknown keys are rejected.

## Running it

```sh
grokptah-headless config-check --config ./headless.json
grokptah-headless health        --config ./headless.json
grokptah-headless serve         --config ./headless.json
```

`serve` speaks newline-delimited JSON on stdin/stdout — one request per line, one
reply per line, diagnostics on stderr. `SIGTERM` or `Ctrl+C` drains and
checkpoints live runs; a second signal stops immediately and leaves the next
start to recover.

## Verifying

```sh
cargo test   -p grokptah-headless-host --all-features --locked
cargo fmt    -p grokptah-headless-host -- --check
cargo clippy -p grokptah-headless-host --all-targets --all-features --locked
```
