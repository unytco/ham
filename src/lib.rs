//! Production-grade Holochain `AppWebsocket` client wrapper.
//!
//! This crate centralizes the patterns every long-running Rust service in the
//! unyt fleet needs when talking to a Holochain conductor:
//!
//! * [`Ham`] &mdash; a thin wrapper around [`holochain_client::AppWebsocket`]
//!   that handles admin-interface discovery, app-interface attach, signing
//!   credential authorization, and typed msgpack zome calls with an explicit
//!   per-request timeout.
//! * [`errors::is_connection_error`] &mdash; string-based classifier that
//!   decides whether an [`anyhow::Error`] warrants rebuilding the socket.
//! * [`reconnect::connect_with_backoff`] and [`reconnect::compute_delay_ms`]
//!   &mdash; shutdown-aware exponential-backoff reconnect loop with jitter.
//! * [`shutdown::install_shutdown_handler`] &mdash; returns a [`ShutdownRx`]
//!   that flips to `true` on SIGINT/SIGTERM.
//!
//! Daemons typically use all four. One-shot CLIs just construct [`Ham`] with
//! a [`HamConfig::request_timeout_secs`] set and skip the rest.

pub mod client;
pub mod errors;
pub mod reconnect;
pub mod shutdown;

pub use client::{Ham, HamConfig};
pub use errors::{is_connection_error, is_source_chain_pressure};
pub use reconnect::{compute_delay_ms, connect_with_backoff, BackoffConfig};
pub use shutdown::{install_shutdown_handler, ShutdownRx};
