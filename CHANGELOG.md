# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `HamConfig::force_fresh_attach` (+ `with_force_fresh_attach` builder): when set, `Ham::connect` skips `list_app_interfaces` discovery and always attaches a fresh `AllowedOrigins::Any` app interface. Works around conductors that retain stale unrestricted app interfaces whose token/origin state rejects an anonymous connect — the default discovery keeps re-picking the same poisoned one on every retry. Default `false` — the discovery path is unchanged.
- Lair signing. `HamConfig::try_lair_signing_from_node` (discovers the lair `connection_url` from a conductor config and reads the passphrase file, stripping a trailing newline) and `HamConfig::with_lair_signing` (explicit URL + passphrase) make `Ham::connect` sign zome calls as the cell's own agent key via lair (`holochain_client::LairAgentSigner`) — the implicit `ChainAuthor` grant — so **no capability grant is committed to the source chain**. The default path is unchanged: without lair config, `Ham::connect` still authorizes a throwaway signing key on chain (one cap grant per connect). Discovery failures log a warning and fall back to that default path rather than erroring. Adds a `lair_keystore_api` dependency and re-exports `LairSigning`.

### Changed

- upgrade holochain_client to 0.8.2-rc.0 for Holochain 0.6.2-rc.0
