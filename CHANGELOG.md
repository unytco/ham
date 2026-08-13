# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `HamConfig::force_fresh_attach` (+ `with_force_fresh_attach` builder) — skip `list_app_interfaces` discovery and always attach a fresh `AllowedOrigins::Any` interface. Default `false`; discovery unchanged.
- Lair signing — `HamConfig::try_lair_signing_from_node` / `with_lair_signing` make `Ham::connect` sign zome calls as the cell's own agent key, so **no capability grant is committed to the source chain**. Without lair config the throwaway-key path is unchanged.

### Changed

- `is_connection_error` now classifies the send-path `tungstenite` close variants (`SendAfterClosing`, `AlreadyClosed`, `ConnectionClosed`, `ResetWithoutClosingHandshake`) — a send-side close reconnects instead of retrying a dead socket. Matching is case-insensitive.
- `is_connection_error` now also classifies `WebsocketError::Other("ResponderDropped")` — a dropped in-flight responder leaves the socket's state unknown, so consumers reconnect instead of retrying on it. Write backpressure (`WriteBufferFull`) and oversize-payload (`Capacity`/message-too-long) errors are deliberately left **unclassified** — a reconnect can't help either case, so consumers must cool down rather than rebuild the socket; give the consumer retry chain a terminal fallback for them (see `raindex-orders` bridge-orchestrator).
- classifier tests now pin every classifier (`is_connection_error`, `is_request_timeout`, `is_source_chain_pressure`) against a **real** upstream error value — the source-chain-pressure path via a new version-unified `holochain_conductor_api` dev-dependency (`=0.7.0`, the version `holochain_client 0.9.0` already resolves). A `holochain_client` / `holochain_websocket` / `holochain_conductor_api` bump that rewords an error attribute now fails the suite instead of silently misrouting a reconnect or a cooldown.
- upgrade holochain_client to `=0.9.0` (and lair_keystore_api to 0.7.1) for Holochain 0.7 — breaking for consumers, who must bump in lockstep.
- upgrade holochain_client to 0.8.2-rc.0 for Holochain 0.6.2-rc.0
