//! Error classification helpers shared across all consumers.

/// Classifies whether an `anyhow::Error` looks like a websocket / transport
/// failure that warrants rebuilding the `Ham` connection. Matches against the
/// rendered error chain so it handles both direct `holochain_client` failures
/// and wrapped context messages.
///
/// This is string-based because `holochain_client 0.8.x` surfaces websocket
/// failures as opaque strings inside `ConductorApiError::WebsocketError(_)`
/// and similar variants. The classifier is covered by unit tests to guard
/// against dependency upgrades silently changing the error text.
pub fn is_connection_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    // IMPORTANT: do NOT use a bare `"Websocket error"` needle here. That
    // phrase is emitted for both genuine transport failures AND for
    // per-request timeouts (`"Websocket error: Timeout"`), and the latter
    // must NOT trigger a reconnect — the socket is still healthy. See
    // [`is_request_timeout`] for that case.
    const NEEDLES: &[&str] = &[
        "Websocket closed",
        "ConnectionClosed",
        "No connection",
        "ResetWithoutClosingHandshake",
        "broken pipe",
        "connection reset",
        "IO error",
    ];
    NEEDLES.iter().any(|n| msg.contains(n))
}

/// Classifies whether an `anyhow::Error` is a *per-request* timeout from the
/// Holochain app websocket — i.e. the client's `default_request_timeout`
/// fired because a single zome call didn't return in time, while the socket
/// itself remained healthy.
///
/// `holochain_client 0.8.x` surfaces this case as `"Websocket error: Timeout"`
/// (distinct from `"Websocket error: Websocket closed: …"` and the other
/// transport failures classified by [`is_connection_error`]).
///
/// The right reaction is a short cooldown and retry on the *existing*
/// connection, not a reconnect: dropping and rebuilding the socket to
/// recover from a slow call does nothing useful and costs a fresh
/// admin-interface handshake.
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
}
