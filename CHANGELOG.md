# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `HamConfig::with_signing` and `SigningPolicy`: the lair-or-refuse decision, made here once from a caller's own inputs. Supply `LairCredentials::Values` for values you already resolved or `LairCredentials::Node` for the conductor's paths, plus a `CapGrantOptIn` that `from_value` parses from an operator-supplied value and `from_flag` from a parsed flag. The refusal carries the generic fault, and the caller names its own variables or flags as `anyhow` context.
- `HamConfig::force_fresh_attach` (+ `with_force_fresh_attach` builder) — skip `list_app_interfaces` discovery and always attach a fresh `AllowedOrigins::Any` interface. Default `false`; discovery unchanged.
- Lair signing: `Ham::connect` signs zome calls as the cell's own agent key, so **no capability grant is committed to the source chain**.

### Removed

- `HamConfig::try_lair_signing_from_node`. Its "lair when the node has it, the chain write when it does not" is now `with_signing(LairCredentials::Node { .. }, CapGrantOptIn::Permitted)`, which states the fallback instead of leaving a silently refusing config behind.
- `HamConfig::with_lair_signing_from_node`, which was `with_signing(LairCredentials::Node { .. }, CapGrantOptIn::Withheld)` spelled a second way.

### Changed

- `CapGrantOptIn::Permitted` covers one case: there is no lair to reach, which for a `Node` means a conductor running the in-process or test keystore, or neither of its files on the node at all. Anything supplied that cannot be used is fatal whichever way the opt-in is set, so a typo can never become a capability grant committed to the agent's chain: a config the conductor would refuse to load is read as a mistake to fix, never as a node without lair. That config is read with the `ConductorConfig` ham is pinned to, so one written for another Holochain line refuses rather than falls back.
- A conductor config path pointed at a file that names no keystore section, the lair passphrase file for instance, is refused without rendering any of that file's contents into the error, which a reconnect loop logs on every attempt.
- `ham.cap_grant_unused` logs at `info`, not `warn`: a caller that permits the capability grant and has lair too is signing through lair, which is the intended outcome and not a fault.
- `Ham::connect` refuses a config that has neither lair signing nor `HamConfig::allow_cap_grant_signing()`, so connecting never commits a capability grant nobody asked for. A node that cannot offer lair reports why instead of falling back to that write, and `is_signing_refusal` tells a reconnect loop that a refusal is a config to fix rather than a conductor to wait for.
- `is_connection_error` now classifies the send-path `tungstenite` close variants (`SendAfterClosing`, `AlreadyClosed`, `ConnectionClosed`, `ResetWithoutClosingHandshake`) — a send-side close reconnects instead of retrying a dead socket. Matching is case-insensitive.
- `is_connection_error` classifies `ResponderDropped` as a connection error.
- Pin the error classifiers against real upstream error values.
- upgrade holochain_client to `=0.9.0` (and lair_keystore_api to 0.7.1) for Holochain 0.7 — breaking for consumers, who must bump in lockstep.
- upgrade holochain_client to 0.8.2-rc.0 for Holochain 0.6.2-rc.0
