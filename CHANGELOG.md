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
- `is_connection_error` classifies `ResponderDropped` as a connection error.
- Pin the error classifiers against real upstream error values.
- upgrade holochain_client to `=0.9.0` (and lair_keystore_api to 0.7.1) for Holochain 0.7 — breaking for consumers, who must bump in lockstep.
- upgrade holochain_client to 0.8.2-rc.0 for Holochain 0.6.2-rc.0
