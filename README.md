# ham

Production-grade Holochain `AppWebsocket` client wrapper used by the unyt
server-side services (bridge orchestrator, unyt_cli daemon, pricing oracle,
watchtower).

## What it provides

- `Ham` &mdash; a connect-once wrapper around `holochain_client::AppWebsocket`
  that handles admin-interface discovery, app-interface attach, zome-call
  signing, and typed msgpack zome calls with an explicit per-request timeout.
  Signs via lair as the cell's own agent key, committing no capability grant to
  the chain (`HamConfig::with_lair_signing_from_node` / `with_lair_signing`).
  The other path, authorizing a throwaway signing key by committing one cap
  grant per connect, is reachable only through
  `HamConfig::allow_cap_grant_signing()`: a config with neither is refused
  before `Ham::connect` opens a socket, so connecting never writes to a chain
  no caller asked it to write to.
- `errors::is_connection_error(&anyhow::Error) -> bool` &mdash; string-based
  classifier that decides whether an error warrants rebuilding the socket
  (covered by unit tests). `errors::is_signing_refusal` is its opposite: a
  config no retry can fix, so the caller should stop rather than wait.
- `reconnect::connect_with_backoff` &mdash; shutdown-aware exponential-backoff
  reconnect loop with jitter and log-level escalation. `compute_delay_ms` is
  exposed as a pure function for testing.
- `shutdown::install_shutdown_handler()` &mdash; returns a `ShutdownRx`
  (`tokio::sync::watch::Receiver<bool>`) that flips to `true` on SIGINT or
  SIGTERM.

## Usage

```rust
use ham::{Ham, HamConfig, BackoffConfig, install_shutdown_handler, connect_with_backoff};
use std::path::Path;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut shutdown = install_shutdown_handler();
    let backoff = BackoffConfig::default();

    let cfg = HamConfig::new(30000, 30001, "bridging-app")
        .with_request_timeout_secs(120)
        .with_lair_signing_from_node(
            Path::new("/etc/holochain/conductor-config.yaml"),
            Path::new("/var/lib/holochain/lair-passphrase"),
        )?;

    let mut ham = match connect_with_backoff(
        || Ham::connect(cfg.clone()),
        &backoff,
        &mut shutdown,
    ).await {
        Some(h) => h,
        None => return Ok(()),
    };

    loop {
        if *shutdown.borrow() { break }
        if let Err(e) = ham.ping().await {
            if ham::is_connection_error(&e) {
                if let Some(h) = connect_with_backoff(
                    || Ham::connect(cfg.clone()),
                    &backoff,
                    &mut shutdown,
                ).await {
                    ham = h;
                } else {
                    break;
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
            _ = shutdown.changed() => break,
        }
    }
    Ok(())
}
```

## Holochain client version

This crate pins `holochain_client = "=0.9.0"` exactly (the Holochain 0.7 line). All consumers must align to the same `holochain_client` version because its types flow across the `ham` crate boundary. Lair signing additionally uses `lair_keystore_api = "0.7.1"` (the version `holochain_client` 0.9.0 resolves) to open the keystore connection for the built-in `holochain_client::LairAgentSigner`.

## Tracing event names

The crate emits structured events with stable `event` field names that
deployment dashboards can alert on:

| Event | Level | When |
| --- | --- | --- |
| `ham.connecting` | `info` | `Ham::connect` is invoked and the signing path is settled. |
| `ham.connected` | `info` | App websocket connected and signing set up; the `signing` field is `lair` (no cap grant) or `client` (cap grant committed). |
| `ham.connect.refused` | `error` | A reconnect attempt failed because the config has no signing path that avoids writing to the chain. Logged from the first attempt: retrying cannot clear it. |
| `ham.cap_grant_unused` | `warn` | The config permits a capability grant and has lair too, so lair was used and nothing was written. |
| `ham.cap_grant` | `warn` | About to commit the capability grant `allow_cap_grant_signing` asked for. |
| `ham.lair_discovery_failed` | `warn` | `try_lair_signing_from_node` could not resolve the URL/passphrase; the connect is refused unless the caller also opted in. |
| `ham.call_zome` | `debug` | Per zome call. |
| `ham.reconnect.attempt` | `warn` / `error` | Each failed reconnect attempt (`error` after `escalate_after`). |
| `ham.reconnected` | `info` | Reconnect succeeded after one or more failed attempts. |

Daemons using `connect_with_backoff` typically also emit their own
`ham.disconnected` / `ham.probe.failed` events at the call sites.

## Versioning

Semver from 0.1.0. Consumers pin `rev = "<sha>"` (not a tag) so rollouts are
reproducible; tags are cut once a compatible set of consumer updates has
landed.
