//! Error classification helpers shared across all consumers.

/// Classifies whether an `anyhow::Error` looks like a websocket / transport
/// failure that warrants rebuilding the `Ham` connection. Matches against the
/// rendered error chain so it handles both direct `holochain_client` failures
/// and wrapped context messages.
///
/// This is string-based because `holochain_client 0.9.x` surfaces websocket
/// failures as opaque strings inside `ConductorApiError::WebsocketError(_)`
/// and similar variants. The `upstream_error_text` tests build real upstream
/// error values, so a bump that reworded one of the paths they cover fails the
/// suite — see that module for which paths those are, and which are still only
/// covered by the hand-written strings below.
pub fn is_connection_error(err: &anyhow::Error) -> bool {
    // Lowercased before matching, so the needles below are all lowercase: this
    // classifier alone aggregates text from layers that capitalize differently
    // — tungstenite's `"Connection reset without closing handshake"` vs. an io
    // error's `"connection reset by peer"`. Its two siblings below stay
    // case-sensitive on purpose: each matches one exact upstream `Display`
    // composition, where the case is part of what pins that composition.
    let msg = format!("{err:#}").to_lowercase();
    // IMPORTANT: do NOT use a bare `"websocket error"` needle here. That
    // phrase is emitted for both genuine transport failures AND for
    // per-request timeouts (`"Websocket error: Timeout"`), and the latter
    // must NOT trigger a reconnect — the socket is still healthy. See
    // [`is_request_timeout`] for that case.
    const NEEDLES: &[&str] = &[
        "websocket closed",
        "no connection",
        "broken pipe",
        "connection reset",
        "io error",
        // Post-handshake tungstenite owns the stream, so its own text arrives
        // as a `WebsocketError::Websocket(_)` passthrough sharing no substring
        // with the needles above. A send walks these as the socket goes down:
        // `SendAfterClosing` once either side's close frame has landed,
        // `AlreadyClosed` once it is terminated, and `ConnectionClosed` when
        // the send syscall itself resets while the state says we can no longer
        // read.
        "sending after closing",
        "trying to work with closed connection",
        "connection closed",
        // Retained defensively: no `holochain_websocket 0.7` path renders a
        // tungstenite error with `{:?}`, so these CamelCase variant names only
        // reach us from an older or hand-rolled rendering.
        "connectionclosed",
        "resetwithoutclosinghandshake",
    ];
    NEEDLES.iter().any(|n| msg.contains(n))
}

/// Classifies whether an `anyhow::Error` is a *per-request* timeout from the
/// Holochain app websocket — i.e. the client's `default_request_timeout`
/// fired because a single zome call didn't return in time, while the socket
/// itself remained healthy.
///
/// `holochain_client 0.9.x` surfaces this case as `"Websocket error: Timeout"`
/// (distinct from `"Websocket error: Websocket closed: …"` and the other
/// transport failures classified by [`is_connection_error`]).
///
/// The right reaction is a short cooldown and retry on the *existing*
/// connection, not a reconnect: dropping and rebuilding the socket to
/// recover from a slow call does nothing useful and costs a fresh
/// admin-interface handshake.
///
/// Caveat: `holochain_websocket` raises `Timeout` while awaiting the response
/// (socket healthy, as above) *and* while sending, where the core has already
/// been torn down. Both render identically, so a send-phase timeout advises a
/// cooldown on a dead connection. Self-correcting — the retry then gets
/// `Websocket closed: No connection` and reconnects.
pub fn is_request_timeout(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.contains("Websocket error: Timeout")
}

/// Classifies whether an `anyhow::Error` represents server-side *source-chain
/// pressure* on the Holochain conductor, as opposed to a transport failure.
///
/// The canonical example is `"Source chain error: deadline has elapsed"`:
/// the workflow hit its internal timeout while the websocket was still
/// healthy. On these errors the remote commit may or may not have landed,
/// so the caller should back off briefly before retrying rather than
/// hammering a struggling conductor in a tight loop.
///
/// This is intentionally kept distinct from [`is_connection_error`]; the
/// two classes overlap zero in practice and deserve different handling
/// (reconnect vs. cooldown).
pub fn is_source_chain_pressure(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.contains("deadline has elapsed") || msg.contains("Source chain error")
}

#[cfg(test)]
mod tests {
    use super::{is_connection_error, is_request_timeout, is_source_chain_pressure};
    use anyhow::anyhow;

    fn wrap(base: &'static str) -> anyhow::Error {
        anyhow!(base).context("Failed to call zome")
    }

    #[test]
    fn classifies_websocket_closed() {
        let e = wrap("Websocket error: Websocket closed: ConnectionClosed");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn classifies_no_connection() {
        let e = wrap("Websocket error: Websocket closed: No connection");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn classifies_reset_without_closing_handshake() {
        // Observed on the conductor side when the orchestrator drops the
        // socket after a client-side request timeout fires.
        let e = wrap("Websocket error: ResetWithoutClosingHandshake");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn classifies_broken_pipe() {
        let e = wrap("io error: broken pipe");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn classifies_connection_reset() {
        let e = wrap("io error: connection reset by peer");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn classifies_generic_io_error() {
        let e = wrap("IO error: unexpected eof");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn classifies_connection_closed_token() {
        let e = anyhow!("ConnectionClosed");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn rejects_decode_error() {
        let e = wrap("Failed to deserialize response: invalid type");
        assert!(!is_connection_error(&e));
    }

    #[test]
    fn rejects_zome_logic_error() {
        let e = wrap("Failed to call zome: guest error: validation failed");
        assert!(!is_connection_error(&e));
    }

    #[test]
    fn rejects_unrelated_error() {
        let e = anyhow!("some unrelated problem");
        assert!(!is_connection_error(&e));
    }

    #[test]
    fn bare_websocket_error_is_not_a_connection_error() {
        // A "Websocket error: …" prefix without a concrete transport failure
        // (closed / reset / broken pipe / IO error) must NOT be classified as
        // a connection failure. The canonical example is the per-request
        // timeout, which is handled by `is_request_timeout`.
        let e = wrap("Websocket error: some transport failure");
        assert!(!is_connection_error(&e));
    }

    #[test]
    fn classifies_websocket_timeout_as_request_timeout_not_connection() {
        // Exact error string emitted by holochain_client when the app-websocket
        // `default_request_timeout` fires while the socket itself is healthy.
        let e = wrap("Websocket error: Timeout");
        assert!(is_request_timeout(&e));
        assert!(!is_connection_error(&e));
        // And not server-side source-chain pressure either — it's a
        // client-side per-request budget.
        assert!(!is_source_chain_pressure(&e));
    }

    #[test]
    fn rejects_transport_failures_as_request_timeout() {
        let e = wrap("Websocket error: Websocket closed: ConnectionClosed");
        assert!(!is_request_timeout(&e));
    }

    #[test]
    fn rejects_unrelated_error_as_request_timeout() {
        let e = anyhow!("some unrelated problem");
        assert!(!is_request_timeout(&e));
    }

    #[test]
    fn classifies_deadline_elapsed_as_source_chain_pressure() {
        // Exact error string from the incident that motivated this classifier.
        let e = wrap("Source chain error: deadline has elapsed");
        assert!(is_source_chain_pressure(&e));
        // And is NOT treated as a socket failure — the socket is fine.
        assert!(!is_connection_error(&e));
    }

    #[test]
    fn classifies_bare_deadline_elapsed() {
        let e = anyhow!("deadline has elapsed");
        assert!(is_source_chain_pressure(&e));
    }

    #[test]
    fn classifies_bare_source_chain_error() {
        let e = wrap("Source chain error: some other backpressure mode");
        assert!(is_source_chain_pressure(&e));
    }

    #[test]
    fn rejects_connection_error_as_source_chain_pressure() {
        let e = wrap("Websocket error: Websocket closed: ConnectionClosed");
        assert!(!is_source_chain_pressure(&e));
    }

    #[test]
    fn rejects_unrelated_error_as_source_chain_pressure() {
        let e = anyhow!("some unrelated problem");
        assert!(!is_source_chain_pressure(&e));
    }

    /// The tests above feed hand-written strings, so they pin only the
    /// classifier. These build the *real* upstream error values and assert the
    /// exact text they render, so a `holochain_client` / `holochain_websocket`
    /// bump that edits an `#[error(...)]` attribute fails here instead of
    /// silently misrouting reconnects in production.
    ///
    /// What they do *not* pin, and so must be re-checked by hand on a bump —
    /// a non-exhaustive list, since neither enum is covered variant by
    /// variant: the `Close(_)` payloads below are literals copied from
    /// `holochain_websocket`'s own call sites rather than from a format
    /// attribute; and `WebsocketError::Other` and
    /// `ConductorApiError::ExternalApiWireError` are uncovered — the latter
    /// renders with `{0:?}`, so `is_source_chain_pressure` rides on an
    /// auto-derived `Debug` that can shift with no `#[error(...)]` edit to
    /// review and no semver signal.
    mod upstream_error_text {
        use super::{is_connection_error, is_request_timeout};
        use holochain_client::ConductorApiError;
        use holochain_websocket::WebsocketError;

        /// Assert what upstream renders, then hand back the error in the shape
        /// production builds it: every `Ham` method wraps with
        /// `anyhow!("…: {}", e)` (`client.rs`), so Display is all the
        /// classifiers ever see.
        #[track_caller]
        fn rendering(e: ConductorApiError, expected: &str) -> anyhow::Error {
            assert_eq!(e.to_string(), expected, "upstream error text changed");
            anyhow::anyhow!("Failed to call zome: {}", e)
        }

        #[tokio::test]
        async fn request_timeout_is_a_timeout_not_a_connection_failure() {
            let elapsed =
                tokio::time::timeout(std::time::Duration::ZERO, std::future::pending::<()>())
                    .await
                    .expect_err("a zero-length timeout must elapse");

            let e = rendering(
                ConductorApiError::WebsocketError(WebsocketError::Timeout(elapsed)),
                "Websocket error: Timeout",
            );
            assert!(is_request_timeout(&e), "got {e:#}");
            assert!(!is_connection_error(&e), "got {e:#}");
        }

        #[test]
        fn in_flight_call_on_a_closing_socket_is_a_connection_failure() {
            // What the pending-request map resolves every outstanding call
            // with when the connection core tears down — not the read half,
            // whose errors never reach a `Ham` caller.
            let e = rendering(
                ConductorApiError::WebsocketError(WebsocketError::Close(
                    "ConnectionClosed".to_string(),
                )),
                "Websocket error: Websocket closed: ConnectionClosed",
            );
            assert!(is_connection_error(&e), "got {e:#}");
            assert!(!is_request_timeout(&e), "got {e:#}");
        }

        #[test]
        fn call_after_the_socket_closed_is_a_connection_failure() {
            // Every later call short-circuits here until `Ham` reconnects.
            let e = rendering(
                ConductorApiError::WebsocketError(WebsocketError::Close(
                    "No connection".to_string(),
                )),
                "Websocket error: Websocket closed: No connection",
            );
            assert!(is_connection_error(&e), "got {e:#}");
        }

        #[test]
        fn mid_session_io_failure_is_a_connection_failure() {
            // Deliberately neither `ConductorApiError::IoError` nor
            // `WebsocketError::Io` — both only arise while *connecting*
            // (address resolution, `TcpStream::connect`), and
            // `connect_with_backoff` retries those without ever consulting the
            // classifier. Post-handshake tungstenite owns the stream, so an io
            // failure raised on the send path arrives wrapped like this —
            // doubled prefix and all — rather than via the map drain above.
            let e = rendering(
                ConductorApiError::WebsocketError(WebsocketError::Websocket(Box::new(
                    tungstenite::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "broken pipe",
                    )),
                ))),
                "Websocket error: Websocket error: IO error: broken pipe",
            );
            assert!(is_connection_error(&e), "got {e:#}");
        }

        /// A peer that vanishes without a closing handshake. This and the
        /// three below are the `Websocket(_)` passthroughs — post-handshake
        /// tungstenite owns the stream, so its `Display` text is all the
        /// classifier sees, never the CamelCase variant names
        /// (`ResetWithoutClosingHandshake` here), which only `{:?}` prints and
        /// no `holochain_websocket 0.7` path uses. Caught by
        /// `"connection reset"` only because the message is lowercased first.
        ///
        /// Pinned defensively rather than as a live path: this variant is
        /// raised only inside `read_message_frame`, and the read half's errors
        /// are dropped by `holochain_client`'s poll task, so on 0.9 it never
        /// reaches a `Ham` caller. The three below are the send-path variants
        /// that do — each only in the window before `close_if_err` tears the
        /// core down, after which calls short-circuit to
        /// `Close("No connection")` instead.
        #[test]
        fn protocol_reset_is_a_connection_failure() {
            let e = rendering(
                ConductorApiError::WebsocketError(WebsocketError::Websocket(Box::new(
                    tungstenite::Error::Protocol(
                        tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                    ),
                ))),
                "Websocket error: Websocket error: WebSocket protocol error: \
                 Connection reset without closing handshake",
            );
            assert!(is_connection_error(&e), "got {e:#}");
            assert!(!is_request_timeout(&e), "got {e:#}");
        }

        #[test]
        fn send_on_a_half_closed_socket_is_a_connection_failure() {
            // Either side's close frame has landed — the peer's, or our own
            // during shutdown — but the socket is not terminated yet. What a
            // send hits *before* `AlreadyClosed`, and so the likeliest.
            let e = rendering(
                ConductorApiError::WebsocketError(WebsocketError::Websocket(Box::new(
                    tungstenite::Error::Protocol(
                        tungstenite::error::ProtocolError::SendAfterClosing,
                    ),
                ))),
                "Websocket error: Websocket error: WebSocket protocol error: \
                 Sending after closing is not allowed",
            );
            assert!(is_connection_error(&e), "got {e:#}");
            assert!(!is_request_timeout(&e), "got {e:#}");
        }

        #[test]
        fn use_after_close_is_a_connection_failure() {
            // The socket is fully terminated. Upstream calls this a caller
            // bug; for `Ham` it is the same fact — only a rebuild recovers.
            let e = rendering(
                ConductorApiError::WebsocketError(WebsocketError::Websocket(Box::new(
                    tungstenite::Error::AlreadyClosed,
                ))),
                "Websocket error: Websocket error: Trying to work with closed connection",
            );
            assert!(is_connection_error(&e), "got {e:#}");
            assert!(!is_request_timeout(&e), "got {e:#}");
        }

        #[test]
        fn peer_closed_the_socket_is_a_connection_failure() {
            // Not one of `write`'s own gates: the send syscall resets, and
            // because the state says we can no longer read, tungstenite
            // reports that reset as a clean close (`check_connection_reset`).
            // It survives the sink because `poll_ready`'s flush is the one
            // path that doesn't swallow it — `poll_flush` and `poll_close`
            // both map it to `Ok(())`.
            let e = rendering(
                ConductorApiError::WebsocketError(WebsocketError::Websocket(Box::new(
                    tungstenite::Error::ConnectionClosed,
                ))),
                "Websocket error: Websocket error: Connection closed normally",
            );
            assert!(is_connection_error(&e), "got {e:#}");
            assert!(!is_request_timeout(&e), "got {e:#}");
        }

        #[test]
        fn a_signing_failure_is_not_a_connection_failure() {
            // The negative side pinned to a real upstream value:
            // `is_connection_error` is consulted first in every consumer's
            // retry chain, so a future needle that over-broadens would
            // silently steal cases from the cooldown branch. A signing failure
            // is the clean counter-example — a real `ConductorApiError` whose
            // text comes from an `#[error(...)]` attribute, on a healthy
            // socket that must NOT be rebuilt.
            let e = rendering(
                ConductorApiError::SignZomeCallError(
                    "lair keystore returned no signature".to_string(),
                ),
                "Unable to sign zome call: lair keystore returned no signature",
            );
            assert!(!is_connection_error(&e), "got {e:#}");
            assert!(!is_request_timeout(&e), "got {e:#}");
        }
    }
}
