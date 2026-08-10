//! Per-chain polling: two tokio tasks per configured chain (a cheap "stats"
//! loop and a full "pool" loop), both feeding a single shared channel of
//! [`ChainEvent`].
//!
//! Cadence: on success a task waits its configured interval before trying
//! again; on failure it backs off (1s, 2s, 4s, ... capped at 60s, reset on
//! the next success) and emits [`ChainPayload::Failed`] with the countdown
//! so the UI can show a live retry estimate. The pool task additionally
//! treats [`AdapterError::TooLarge`] / [`AdapterError::Unsupported`] as a
//! "degraded" (stats-only) condition rather than a transient failure: it
//! emits [`ChainPayload::Degraded`] and retries on a much slower, fixed
//! cadence, while the stats task keeps running normally.
//!
//! Tasks never panic on adapter errors — every `Result` from the adapter is
//! matched explicitly and turned into an event.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use poolwatch_core::{AdapterError, ChainAdapter, MempoolSnapshot, PoolStats};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// One update from a chain's polling tasks.
#[derive(Debug, Clone)]
pub struct ChainEvent {
    pub chain_id: String,
    pub payload: ChainPayload,
}

#[derive(Debug, Clone)]
pub enum ChainPayload {
    /// A full pool fetch succeeded.
    Snapshot(MempoolSnapshot),
    /// A cheap stats fetch succeeded.
    Stats(PoolStats),
    /// A fetch (either task) failed; retry is scheduled `next_retry_secs`
    /// from now, following the backoff schedule.
    Failed { error: String, next_retry_secs: u64 },
    /// The pool task hit a capability wall (response too large, or the
    /// endpoint is unsupported by this daemon) and is falling back to
    /// stats-only mode. The stats task is unaffected.
    Degraded { reason: String },
}

/// Backoff schedule shared by both tasks: 1s, 2s, 4s, ... capped at 60s,
/// reset to 1s on the next success.
struct Backoff {
    next: Duration,
}

impl Backoff {
    const START: Duration = Duration::from_secs(1);
    const CAP: Duration = Duration::from_secs(60);

    fn new() -> Self {
        Self { next: Self::START }
    }

    fn reset(&mut self) {
        self.next = Self::START;
    }

    /// Returns the delay to use now, and advances the schedule for next time.
    fn advance(&mut self) -> Duration {
        let delay = self.next;
        self.next = (self.next * 2).min(Self::CAP);
        delay
    }
}

/// Handles for a running poller: `cancel` stops every task, `paused`
/// pauses/resumes all of them, `tasks` can be awaited during shutdown.
pub struct PollerHandles {
    pub cancel: CancellationToken,
    pub paused: Arc<AtomicBool>,
    pub tasks: Vec<JoinHandle<()>>,
}

/// How often a paused task rechecks whether it should resume.
const PAUSE_RECHECK: Duration = Duration::from_millis(200);

/// How much longer than the normal poll interval to wait between retries of
/// a degraded (too-large / unsupported) full-pool fetch (~1 minute at the
/// default 5s poll interval).
const DEGRADED_RETRY_MULTIPLIER: u32 = 12;

/// Spawns the stats + pool tasks for every adapter. Returns the receiving
/// end of the shared event channel plus handles to pause/cancel/await the
/// tasks.
pub fn spawn(
    adapters: Vec<Arc<dyn ChainAdapter>>,
    poll_interval: Duration,
    stats_interval: Duration,
) -> (mpsc::Receiver<ChainEvent>, PollerHandles) {
    let (tx, rx) = mpsc::channel(256);
    let cancel = CancellationToken::new();
    let paused = Arc::new(AtomicBool::new(false));
    let mut tasks = Vec::with_capacity(adapters.len() * 2);

    for adapter in adapters {
        let chain_id = adapter.chain_meta().id.clone();

        tasks.push(tokio::spawn(stats_loop(
            chain_id.clone(),
            Arc::clone(&adapter),
            stats_interval,
            tx.clone(),
            Arc::clone(&paused),
            cancel.clone(),
        )));

        tasks.push(tokio::spawn(pool_loop(
            chain_id,
            adapter,
            poll_interval,
            tx.clone(),
            Arc::clone(&paused),
            cancel.clone(),
        )));
    }

    (
        rx,
        PollerHandles {
            cancel,
            paused,
            tasks,
        },
    )
}

async fn stats_loop(
    chain_id: String,
    adapter: Arc<dyn ChainAdapter>,
    interval: Duration,
    tx: mpsc::Sender<ChainEvent>,
    paused: Arc<AtomicBool>,
    cancel: CancellationToken,
) {
    let mut backoff = Backoff::new();
    let mut delay = Duration::ZERO; // fetch immediately on startup

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(delay) => {}
        }

        if paused.load(Ordering::Relaxed) {
            delay = PAUSE_RECHECK;
            continue;
        }

        match adapter.fetch_pool_stats().await {
            Ok(stats) => {
                backoff.reset();
                delay = interval;
                if send(&tx, &chain_id, ChainPayload::Stats(stats))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(err) => {
                delay = backoff.advance();
                warn!(chain = %chain_id, error = %err, "stats fetch failed");
                let payload = ChainPayload::Failed {
                    error: err.to_string(),
                    next_retry_secs: delay.as_secs(),
                };
                if send(&tx, &chain_id, payload).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn pool_loop(
    chain_id: String,
    adapter: Arc<dyn ChainAdapter>,
    interval: Duration,
    tx: mpsc::Sender<ChainEvent>,
    paused: Arc<AtomicBool>,
    cancel: CancellationToken,
) {
    let mut backoff = Backoff::new();
    let mut delay = Duration::ZERO; // fetch immediately on startup

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(delay) => {}
        }

        if paused.load(Ordering::Relaxed) {
            delay = PAUSE_RECHECK;
            continue;
        }

        match adapter.fetch_snapshot().await {
            Ok(snapshot) => {
                backoff.reset();
                delay = interval;
                if send(&tx, &chain_id, ChainPayload::Snapshot(snapshot))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(AdapterError::TooLarge { size, limit }) => {
                delay = interval * DEGRADED_RETRY_MULTIPLIER;
                let reason = format!(
                    "pool response too large ({size} bytes, limit {limit}); \
                     stats-only until it shrinks"
                );
                warn!(chain = %chain_id, %reason, "chain degraded");
                if send(&tx, &chain_id, ChainPayload::Degraded { reason })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(AdapterError::Unsupported(detail)) => {
                delay = interval * DEGRADED_RETRY_MULTIPLIER;
                let reason = format!("full pool fetch unsupported by this daemon: {detail}");
                warn!(chain = %chain_id, %reason, "chain degraded");
                if send(&tx, &chain_id, ChainPayload::Degraded { reason })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(err) => {
                delay = backoff.advance();
                warn!(chain = %chain_id, error = %err, "pool fetch failed");
                let payload = ChainPayload::Failed {
                    error: err.to_string(),
                    next_retry_secs: delay.as_secs(),
                };
                if send(&tx, &chain_id, payload).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// Sends one event; `Err(())` means the receiver is gone (app shutting
/// down), telling the caller to stop looping.
async fn send(
    tx: &mpsc::Sender<ChainEvent>,
    chain_id: &str,
    payload: ChainPayload,
) -> Result<(), ()> {
    tx.send(ChainEvent {
        chain_id: chain_id.to_string(),
        payload,
    })
    .await
    .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let mut b = Backoff::new();
        assert_eq!(b.advance(), Duration::from_secs(1));
        assert_eq!(b.advance(), Duration::from_secs(2));
        assert_eq!(b.advance(), Duration::from_secs(4));
        assert_eq!(b.advance(), Duration::from_secs(8));
        assert_eq!(b.advance(), Duration::from_secs(16));
        assert_eq!(b.advance(), Duration::from_secs(32));
        assert_eq!(b.advance(), Duration::from_secs(60)); // capped, would be 64
        assert_eq!(b.advance(), Duration::from_secs(60)); // stays capped
    }

    #[test]
    fn backoff_reset_returns_to_start() {
        let mut b = Backoff::new();
        b.advance();
        b.advance();
        b.reset();
        assert_eq!(b.advance(), Duration::from_secs(1));
    }
}
