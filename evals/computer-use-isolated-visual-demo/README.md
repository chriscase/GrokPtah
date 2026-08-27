# Isolated visual disposable fixture

Repo-owned runner for [#288](https://github.com/chriscase/GrokPtah/issues/288).
This is not a live-desktop demo and not Virtualization.framework qualification.

The fixture exercises the host-owned guest lifecycle, hermetic source resolver,
leases, stale-frame denial, duplicate dispatch, two-restart recovery, cleanup,
and redacted projection through `grokptah-isolated-visual`.

Simulator evidence is **ineligible** for VM qualification.

## Run

```sh
evals/computer-use-isolated-visual-demo/run.sh
```

The script refuses to launch Virtualization.framework. Set
`GROKPTAH_ISOLATED_VISUAL_ALLOW_VIRTUALIZATION=1` only on an eligible machine
with signed artifacts and at least 25 GiB free; even then this runner only
invokes the fail-closed preflight.
