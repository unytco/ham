# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `HamConfig::force_fresh_attach` (+ `with_force_fresh_attach` builder): skip `list_app_interfaces` discovery and always attach a fresh `AllowedOrigins::Any` app interface, for conductors that keep re-picking a stale unrestricted interface whose token/origin state rejects the anonymous connect. Default `false` — discovery unchanged.
- Lair signing: `HamConfig::try_lair_signing_from_node` (discovers the lair `connection_url` from a conductor config and reads the passphrase file) and `with_lair_signing` (explicit URL + passphrase) make `Ham::connect` sign zome calls as the cell's own agent key via lair, so **no capability grant is committed to the source chain**. Without lair config the default throwaway-key path is unchanged; discovery failures warn and fall back. Adds a `lair_keystore_api` dependency and re-exports `LairSigning`.

### Changed

- `is_connection_error` now classifies the send-path `tungstenite` variants a closing socket produces — `SendAfterClosing`, `AlreadyClosed`, `ConnectionClosed`, plus `ResetWithoutClosingHandshake` defensively. These previously arrived as an unmatched `WebsocketError::Websocket(_)` passthrough string, so a send-side close left the caller retrying into a dead socket instead of reconnecting. Matching is now case-insensitive too.
- upgrade holochain_client to 0.9.0, pinned exactly as `=0.9.0` (and lair_keystore_api to 0.7.1) for Holochain 0.7 — breaking for consumers, who must bump in lockstep because these types cross the `ham` API boundary
- upgrade holochain_client to 0.8.2-rc.0 for Holochain 0.6.2-rc.0
