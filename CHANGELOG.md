# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Lair signing. `HamConfig::try_lair_signing_from_node` (discovers the lair `connection_url` from a conductor config and reads the passphrase file, stripping a trailing newline) and `HamConfig::with_lair_signing` (explicit URL + passphrase) make `Ham::connect` sign zome calls as the cell's own agent key via lair (`holochain_client::LairAgentSigner`) — the implicit `ChainAuthor` grant — so **no capability grant is committed to the source chain**. The default path is unchanged: without lair config, `Ham::connect` still authorizes a throwaway signing key on chain (one cap grant per connect). Discovery failures log a warning and fall back to that default path rather than erroring. Adds a `lair_keystore_api` dependency and re-exports `LairSigning`.

### Changed

- upgrade holochain_client to 0.8.1 for Holochain 0.6.1
