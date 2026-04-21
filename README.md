# ham

Production-grade Holochain `AppWebsocket` client wrapper used by the unyt
server-side services (bridge orchestrator, unyt_cli daemon, pricing oracle,
watchtower).

## What it provides

- `Ham` &mdash; a connect-once wrapper around `holochain_client::AppWebsocket`
  that handles admin-interface discovery, app-interface attach, signing
  credential authorization, and typed msgpack zome calls with an explicit
  per-request timeout.
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

This crate currently pins `holochain_client = "0.8.1-rc.7"`. All consumers
must align to the same `holochain_client` rc because cargo treats
pre-release versions as incompatible and `holochain_client` types flow
across the `ham` crate boundary.

## Tracing event names

The crate emits structured events with stable `event` field names that
deployment dashboards can alert on:

| Event | Level | When |
| --- | --- | --- |
| `ham.connecting` | `info` | `Ham::connect` is invoked. |
| `ham.connected` | `info` | Connection, app info, signing credentials all succeeded. |
| `ham.call_zome` | `debug` | Per zome call. |
| `ham.reconnect.attempt` | `warn` / `error` | Each failed reconnect attempt (`error` after `escalate_after`). |
| `ham.reconnected` | `info` | Reconnect succeeded after one or more failed attempts. |

Daemons using `connect_with_backoff` typically also emit their own
`ham.disconnected` / `ham.probe.failed` events at the call sites.

## Versioning

Semver from 0.1.0. Consumers pin `rev = "<sha>"` (not a tag) so rollouts are
reproducible; tags are cut once a compatible set of consumer updates has
landed.
