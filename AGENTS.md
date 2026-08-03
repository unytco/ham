# ham — Agent Instructions

> **This repo follows the workshop root's patterns — it does not define its own.** Development workflow, process, changelog conventions, and spec/feature-doc discipline live in the workshop: [`CLAUDE.md`](../CLAUDE.md), [`AGENTS.md`](../AGENTS.md), [`documentation/DEVELOPMENT_WORKFLOW.md`](../documentation/DEVELOPMENT_WORKFLOW.md). Below is only what's specific to THIS repo.

## Purpose

`library` — production `AppWebsocket` client wrapper used by every Rust service in the Unyt workshop that talks to a Holochain conductor. Wraps `holochain_client::AppWebsocket` and adds connect-once setup, lair or client-side zome-call signing, typed msgpack zome calls with explicit timeouts, shutdown-aware reconnect with backoff/jitter, and a connection-error classifier.

## Stack

- Rust crate (no `flake.nix`, no Nix shell required).
- `tokio` async runtime.
- Pinned to a single `holochain_client` version (`0.9.0`, the Holochain 0.7
  line — see [`Cargo.toml`](./Cargo.toml)).

## Build

```bash
cargo build --release
```

## Format

Apply, then verify:

```bash
cargo fmt
cargo fmt --check
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

## Repo-specific rules

- **Changelog: call out breaking changes explicitly.** Consumers pin `ham` by `rev = "<sha>"`, so entries for public-API renames, error-semantics changes, or dropped trait impls go under `### Changed` / `### Removed`.
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
