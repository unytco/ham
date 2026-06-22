# ham

Production-grade Holochain `AppWebsocket` client wrapper used by the unyt
server-side services (bridge orchestrator, unyt_cli daemon, pricing oracle,
watchtower).

## What it provides

- `Ham` &mdash; a connect-once wrapper around `holochain_client::AppWebsocket`
  that handles admin-interface discovery, app-interface attach, zome-call
  signing, and typed msgpack zome calls with an explicit per-request timeout.
  Signs either via lair as the cell's own agent key &mdash; no capability grant
  committed to the chain (`HamConfig::try_lair_signing_from_node` /
  `with_lair_signing`) &mdash; or, by default, by authorizing a throwaway
  signing key on chain (one cap grant per connect).
- `errors::is_connection_error(&anyhow::Error) -> bool` &mdash; string-based
  classifier that decides whether an error warrants rebuilding the socket
  (covered by unit tests).
- `reconnect::connect_with_backoff` &mdash; shutdown-aware exponential-backoff
  reconnect loop with jitter and log-level escalation. `compute_delay_ms` is
  exposed as a pure function for testing.
- `shutdown::install_shutdown_handler()` &mdash; returns a `ShutdownRx`
  (`tokio::sync::watch::Receiver<bool>`) that flips to `true` on SIGINT or
  SIGTERM.

## Usage

```rust
use ham::{Ham, HamConfig, BackoffConfig, install_shutdown_handler, connect_with_backoff};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut shutdown = install_shutdown_handler();
    let backoff = BackoffConfig::default();

    let cfg = HamConfig::new(30000, 30001, "bridging-app")
        .with_request_timeout_secs(120);

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

This crate pins `holochain_client = "0.8.1"`. All consumers must align to the same `holochain_client` version because its types flow across the `ham` crate boundary. Lair signing additionally uses `lair_keystore_api = "0.6.3"` (the version `holochain_client` 0.8.1 resolves) to open the keystore connection for the built-in `holochain_client::LairAgentSigner`.

## Tracing event names

The crate emits structured events with stable `event` field names that
deployment dashboards can alert on:

| Event | Level | When |
| --- | --- | --- |
| `ham.connecting` | `info` | `Ham::connect` is invoked. |
| `ham.connected` | `info` | App websocket connected and signing set up; the `signing` field is `lair` (no cap grant) or `client` (cap grant committed). |
| `ham.lair_discovery_failed` | `warn` | Lair signing requested but the URL/passphrase couldn't be resolved; fell back to client signing. |
| `ham.call_zome` | `debug` | Per zome call. |
| `ham.reconnect.attempt` | `warn` / `error` | Each failed reconnect attempt (`error` after `escalate_after`). |
| `ham.reconnected` | `info` | Reconnect succeeded after one or more failed attempts. |

Daemons using `connect_with_backoff` typically also emit their own
`ham.disconnected` / `ham.probe.failed` events at the call sites.

## Versioning

Semver from 0.1.0. Consumers pin `rev = "<sha>"` (not a tag) so rollouts are
reproducible; tags are cut once a compatible set of consumer updates has
landed.
