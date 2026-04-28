# ham — Agent Instructions

## Purpose

Production `AppWebsocket` client wrapper used by every Rust service in the
Unyt workshop that talks to a Holochain conductor (`bridge-orchestrator`,
`unyt_cli` daemon, `pricing_oracle`, `watchtower/observer`). Wraps
`holochain_client::AppWebsocket` and adds connect-once setup, signing
credential authorization, typed msgpack zome calls with explicit timeouts,
shutdown-aware reconnect with backoff/jitter, and a connection-error
classifier.

## Classification

`library`

## Stack

- Rust crate (no `flake.nix`, no Nix shell required).
- `tokio` async runtime.
- Pinned to a single `holochain_client` rc (`0.8.1-rc.7` at time of writing
  — see [`Cargo.toml`](./Cargo.toml)).

## Build

```bash
cargo build --release
```

## Test

```bash
cargo test
```

Unit tests cover the connection-error string classifier, the backoff
delay calculator (`compute_delay_ms`), and the shutdown handler.

## Deploy

n/a — this is a library. Consumers pin a `rev = "<sha>"` (not a tag) in
their own `Cargo.toml` so rollouts are reproducible. Tags are cut once a
compatible set of consumer updates has landed.

## Related repos in workshop

- Consumed by [`raindex-orders/bridge-orchestrator`](../raindex-orders/),
  [`unyt-sandbox/unyt/.../unyt_cli`](../unyt-sandbox/),
  [`pricing_oracle`](../pricing_oracle/),
  [`watchtower/crates/observer`](../watchtower/).
- See workshop [`AGENTS.md`](../AGENTS.md) for the full classification map.

## Repo-specific rules

- **`holochain_client` version pin is load-bearing.** Cargo treats
  pre-release versions as incompatible across consumers. Bumping the pin
  here must be paired with simultaneous bumps in every consumer; otherwise
  the workshop won't build. Open a coordinated PR plan before bumping.
- **Stable tracing event names.** Production dashboards alert on
  `ham.connecting`, `ham.connected`, `ham.call_zome`,
  `ham.reconnect.attempt`, `ham.reconnected`. Renaming is a breaking
  change — bump the minor version and notify consumers.
- **`is_connection_error` is string-based by necessity.** It classifies
  `anyhow::Error` strings to decide whether to rebuild the socket. New
  error patterns must come with a unit test. See `errors.rs`.
- **Shutdown-aware everything.** Public APIs that loop or retry must take
  a `ShutdownRx` (or accept one via the call site) so SIGINT/SIGTERM
  cleanly tears down. No infinite loops without a shutdown branch.

## Lessons learned

_Append entries here whenever an agent (or human) loses time to something
a guardrail would have prevented. Keep each entry: date, short symptom,
concrete fix._
