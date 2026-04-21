//! Cross-platform graceful shutdown signalling.

use tokio::sync::watch;
use tracing::{info, warn};

/// Receiver side of the shutdown signal. Set to `true` when the process
/// receives Ctrl+C (SIGINT) or SIGTERM.
///
/// Callers typically interleave with other futures via
/// `tokio::select! { _ = shutdown.changed() => ... }` or check
/// `*shutdown.borrow()` at cycle boundaries.
pub type ShutdownRx = watch::Receiver<bool>;

/// Spawn a background task that flips the returned [`ShutdownRx`] to `true`
/// once the process receives Ctrl+C or SIGTERM. The initial value is marked
/// as seen so subsequent `.changed()` calls only complete on a real signal.
///
/// On unix, installs both SIGINT and SIGTERM handlers. On non-unix targets,
/// only Ctrl+C is observed.
pub fn install_shutdown_handler() -> ShutdownRx {
    let (tx, mut rx) = watch::channel(false);
    rx.mark_unchanged();

    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            match signal(SignalKind::terminate()) {
                Ok(mut sigterm) => {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            info!("received SIGINT, initiating graceful shutdown");
                        }
                        _ = sigterm.recv() => {
                            info!("received SIGTERM, initiating graceful shutdown");
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "failed to install SIGTERM handler, falling back to Ctrl+C only: {}",
                        e
                    );
                    let _ = tokio::signal::ctrl_c().await;
                    info!("received SIGINT, initiating graceful shutdown");
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            info!("received Ctrl+C, initiating graceful shutdown");
        }
        let _ = tx.send(true);
    });

    rx
}
