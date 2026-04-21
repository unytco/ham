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
    const NEEDLES: &[&str] = &[
        "Websocket closed",
        "ConnectionClosed",
        "No connection",
        "Websocket error",
        "broken pipe",
        "connection reset",
        "IO error",
    ];
    NEEDLES.iter().any(|n| msg.contains(n))
}

#[cfg(test)]
mod tests {
    use super::is_connection_error;
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
    fn classifies_bare_websocket_error() {
        let e = wrap("Websocket error: some transport failure");
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
}
