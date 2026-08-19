# VM deployment

The headless service is a normal HTTP MCP server. The checked-in systemd
files provide a small, supervised Linux deployment for a VM or dedicated host.

## Install

Build the release binary from the repository and install it somewhere on the
VM:

```sh
cargo build --release --manifest-path crates/codegen/grokptah-service/Cargo.toml
sudo install -m 0755 \
  crates/codegen/grokptah-service/target/release/grokptah-service \
  /usr/local/bin/grokptah-service
```

Create the service account, state directory, and an allowlisted workspace:

```sh
sudo useradd --system --home-dir /var/lib/grokptah --create-home grokptah
sudo install -d -o grokptah -g grokptah -m 0750 /srv/grokptah/workspace
sudo install -d -o root -g grokptah -m 0750 /etc/grokptah
sudo install -d -o grokptah -g grokptah -m 0750 /var/lib/grokptah
```

Copy the environment template, set a unique token, and protect it:

```sh
sudo install -o root -g grokptah -m 0640 \
  deploy/systemd/grokptah-service.env.example \
  /etc/grokptah/grokptah-service.env
sudo editor /etc/grokptah/grokptah-service.env
```

Install the unit and start it:

```sh
sudo install -m 0644 deploy/systemd/grokptah-service.service \
  /etc/systemd/system/grokptah-service.service
sudo systemctl daemon-reload
sudo systemctl enable --now grokptah-service
```

The unit is intentionally loopback-only. Verify it locally:

```sh
curl http://127.0.0.1:39200/health
curl http://127.0.0.1:39200/ready
sudo systemctl status grokptah-service
sudo journalctl -u grokptah-service -f
```

## Network access

For a remote desktop client, keep GrokPtah bound to loopback and put an HTTPS
reverse proxy in front of it. Forward `/mcp`, `/health`, and `/ready` without
buffering the MCP/SSE stream, and preserve the `Authorization` and
`MCP-Protocol-Version` headers. The service does not terminate TLS itself.

If direct non-loopback binding is unavoidable, set
`GROKPTAH_SERVICE_ALLOW_REMOTE=true`, use a token of at least 24 characters,
and update the unit's `ReadWritePaths` and firewall policy. Health and
readiness are authenticated for non-loopback listeners.

## Updates and recovery

Replace `/usr/local/bin/grokptah-service` and run `sudo systemctl restart
grokptah-service`. The run ledger and event journal live under
`GROKPTAH_HOME`; restart recovery marks unfinished runs interrupted and never
silently resumes model execution. Back up `/var/lib/grokptah` as part of the
VM's normal state backup policy. Credentials are configured separately in
`/etc/grokptah/grokptah-service.env` and are not part of the durable home.

For a consistent backup, stop the service first, copy the complete state
directory, and start it again:

```sh
sudo systemctl stop grokptah-service
sudo tar --xattrs --acls -C /var/lib -czf /var/backups/grokptah-home-$(date +%Y%m%d%H%M%S).tar.gz grokptah
sudo systemctl start grokptah-service
```

Restore only while the service is stopped, preserve ownership and mode bits,
and restore the matching workspace paths before starting. A restored home is
the authoritative copy; do not run two services against it, place it on a
multi-writer network filesystem, or sync it live between devices. After a
restore, verify `/ready` and inspect the journal for interrupted runs before
resuming any persistent Agent explicitly.

The configured workspace paths must match the unit's `ReadWritePaths`. If the
environment file adds another workspace, add the same path to the unit before
reloading systemd.
