//! Shutdown-aware exponential-backoff reconnect primitives.

use crate::client::Ham;
use crate::errors::is_signing_refusal;
use crate::shutdown::ShutdownRx;
use std::future::Future;
use std::time::Duration;
use tracing::{error, info, warn};

/// How [`connect_with_backoff`] reports one failed attempt. Split out of the
/// loop so the decision is pinned by a test rather than by reading log output.
#[derive(Debug, PartialEq, Eq)]
enum AttemptReport {
    /// The config has no signing path that avoids writing to the agent's
    /// chain. No retry can clear it, so it is loud from the first attempt.
    Refused,
    /// Enough consecutive failures that an operator should be told.
    Persistent,
    /// A failure the loop is expected to ride out.
    Transient,
}

fn report_for(err: &anyhow::Error, attempt: u32, cfg: &BackoffConfig) -> AttemptReport {
    if is_signing_refusal(err) {
        AttemptReport::Refused
    } else if attempt >= cfg.escalate_after {
        AttemptReport::Persistent
    } else {
        AttemptReport::Transient
    }
}

/// Configuration for [`connect_with_backoff`] and [`compute_delay_ms`].
#[derive(Debug, Clone)]
pub struct BackoffConfig {
    /// Initial delay between the first failed attempt and the retry.
    pub initial_ms: u64,
    /// Cap on the exponential growth.
    pub max_ms: u64,
    /// Number of consecutive failed attempts before escalating log level
    /// from `warn!` to `error!`. The retry loop never gives up on its own;
    /// callers exit by signalling shutdown.
    pub escalate_after: u32,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial_ms: 1000,
            max_ms: 30_000,
            escalate_after: 5,
        }
    }
}

/// Loop forever (until shutdown) trying to establish a fresh [`Ham`] via the
/// provided async factory. Uses exponential backoff capped at `cfg.max_ms`
/// with up to 10% jitter derived from the wall clock's sub-second nanos.
///
/// Each failed attempt logs at `warn!` up to `cfg.escalate_after`, then at
/// `error!` so operator alerts can fire while the loop keeps retrying.
///
/// Returns `None` if `shutdown` flips to `true` while we are sleeping or
/// trying to connect, letting the caller exit cleanly without further I/O.
///
/// A [`crate::errors::is_signing_refusal`] failure can never succeed on a
/// retry, so it is logged at `error!` from the first attempt rather than
/// warned about and escalated later. The loop still retries it: returning
/// `None` is how shutdown is reported, and a caller cannot tell the two apart.
/// Resolve the signing config before entering this loop and a refusal never
/// reaches it.
pub async fn connect_with_backoff<F, Fut>(
    factory: F,
    cfg: &BackoffConfig,
    shutdown: &mut ShutdownRx,
) -> Option<Ham>
where
    F: Fn() -> Fut,
    Fut: Future<Output = anyhow::Result<Ham>>,
{
    let mut attempt: u32 = 0;
    loop {
        if *shutdown.borrow() {
            return None;
        }
        match factory().await {
            Ok(ham) => {
                if attempt > 0 {
                    info!(event = "ham.reconnected", attempts = attempt);
                }
                return Some(ham);
            }
            Err(e) => {
                let delay_ms = compute_delay_ms(attempt, cfg);
                match report_for(&e, attempt, cfg) {
                    AttemptReport::Refused => error!(
                        event = "ham.connect.refused",
                        attempt,
                        delay_ms,
                        error = %e,
                        "connect refused by configuration; no retry can fix this, the config has to change"
                    ),
                    AttemptReport::Persistent => error!(
                        event = "ham.reconnect.attempt",
                        attempt,
                        delay_ms,
                        error = %e,
                        "reconnect failing persistently; operator attention needed"
                    ),
                    AttemptReport::Transient => warn!(
                        event = "ham.reconnect.attempt",
                        attempt,
                        delay_ms,
                        error = %e,
                    ),
                }
                attempt = attempt.saturating_add(1);

                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
                    _ = shutdown.changed() => { return None; }
                }
            }
        }
    }
}

/// Compute the next reconnect delay: exponential (1,2,4,... *initial) capped
/// at `cfg.max_ms`, plus up to 10% jitter seeded from the system clock's
/// sub-second nanos (no extra crate dependency required). Pure function,
/// unit-tested.
pub fn compute_delay_ms(attempt: u32, cfg: &BackoffConfig) -> u64 {
    let shift = attempt.min(20);
    let factor = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let base = cfg.initial_ms.saturating_mul(factor);
    let capped = base.min(cfg.max_ms);
    let jitter_range = (capped / 10).max(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    capped.saturating_add(nanos % jitter_range)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_refusal_is_loud_from_the_very_first_attempt() {
        let err = crate::client::refused_connect_error().await;
        for attempt in [0, 1, cfg().escalate_after, 99] {
            assert_eq!(
                report_for(&err, attempt, &cfg()),
                AttemptReport::Refused,
                "attempt {attempt}"
            );
        }
    }

    #[test]
    fn a_transport_failure_escalates_only_once_it_persists() {
        let err = anyhow::anyhow!("Websocket error: Websocket closed: No connection");
        assert_eq!(report_for(&err, 0, &cfg()), AttemptReport::Transient);
        assert_eq!(
            report_for(&err, cfg().escalate_after - 1, &cfg()),
            AttemptReport::Transient
        );
        assert_eq!(
            report_for(&err, cfg().escalate_after, &cfg()),
            AttemptReport::Persistent
        );
    }

    fn cfg() -> BackoffConfig {
        BackoffConfig {
            initial_ms: 1000,
            max_ms: 30_000,
            escalate_after: 5,
        }
    }

    #[test]
    fn delay_starts_near_initial() {
        let c = cfg();
        let d = compute_delay_ms(0, &c);
        assert!(d >= c.initial_ms);
        assert!(d <= c.initial_ms + (c.initial_ms / 10).max(1));
    }

    #[test]
    fn delay_doubles_until_cap() {
        let c = cfg();
        let d0 = compute_delay_ms(0, &c);
        let d1 = compute_delay_ms(1, &c);
        let d2 = compute_delay_ms(2, &c);
        let floor1 = 2 * c.initial_ms;
        let floor2 = 4 * c.initial_ms;
        assert!(d0 < floor1 + (floor1 / 10).max(1));
        assert!(d1 >= floor1);
        assert!(d2 >= floor2);
    }

    #[test]
    fn delay_capped() {
        let c = cfg();
        let d = compute_delay_ms(20, &c);
        assert!(d >= c.max_ms);
        assert!(d <= c.max_ms + (c.max_ms / 10).max(1));
    }

    #[test]
    fn delay_handles_huge_attempt_without_overflow() {
        let c = cfg();
        let d = compute_delay_ms(u32::MAX, &c);
        assert!(d <= c.max_ms + (c.max_ms / 10).max(1));
    }
}
