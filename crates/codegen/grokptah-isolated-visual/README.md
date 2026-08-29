# grokptah-isolated-visual

Host-owned isolated Computer Use guest/VM lifecycle for [#288](https://github.com/chriscase/GrokPtah/issues/288)
and lease/conflict-domain rules from [#363](https://github.com/chriscase/GrokPtah/issues/363).

This crate is a **source and simulator candidate**. It does not qualify a
Virtualization.framework boot. Simulator evidence and source compilation are
labeled ineligible for VM qualification.

## Authority

The trusted host owns guest identity, source/image manifest, helper identity,
surface incarnation, input/conflict domain, lease issuance, dispatch ordering,
revocation, cleanup, and public projection. Callers and backends cannot
self-attest a surface, domain, lease, or dispatch.

Identifiers bind to the canonical Computer Run / Work / WorkAttempt model:

- one Agent / WorkAttempt / Computer Run per guest lease
- host-issued conflict domain (`conflict-isolated-*`; live desktop remains capacity 1)
- monotonic lease revisions and stable `dispatch_id`
- exactly-once input receipts
- no automatic resume of an old incarnation after restart

## Capacity gate

With less than 25 GiB free, this crate must not create a guest image or a
broad Rust target. Lightweight tests of this crate are the intended proof.

## Commands

```sh
CARGO_TARGET_DIR=/tmp/grokptah-isolated-visual-target \
  cargo test --locked --manifest-path crates/codegen/grokptah-isolated-visual/Cargo.toml

CARGO_TARGET_DIR=/tmp/grokptah-isolated-visual-target \
  cargo run --locked --manifest-path crates/codegen/grokptah-isolated-visual/Cargo.toml \
  --bin grokptah-isolated-visual-qualify -- --out /tmp/isolated-visual-evidence.json
```
