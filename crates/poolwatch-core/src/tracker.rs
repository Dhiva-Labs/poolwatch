//! First-seen tracking and pool-dynamics derivation.
//!
//! The tracker maintains a per-chain map of txid -> first-seen timestamp,
//! pruned when txs leave the pool (mined or dropped). From consecutive
//! snapshots it derives what daemons don't expose: inflow rate (tx/min over
//! rolling 1m/5m/15m windows), median residency of the current pool, and a
//! count of departed txs.

use std::time::{Duration, SystemTime};

use crate::model::MempoolSnapshot;

/// Rolling per-chain state. One instance per configured chain.
#[derive(Debug, Default)]
pub struct ChainTracker {
    // Implementation owned by the tracker/stats task; see contract in
    // `apply` and `stats` docs. Fields are private and may change freely.
}

/// Derived dynamics for one chain, computed against `now`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackerStats {
    /// Transactions per minute, measured over the last 1 minute.
    pub inflow_1m: f64,
    /// Transactions per minute, averaged over the last 5 minutes.
    pub inflow_5m: f64,
    /// Transactions per minute, averaged over the last 15 minutes.
    pub inflow_15m: f64,
    /// Median age of the txs currently in the pool (None when pool empty).
    pub median_residency: Option<Duration>,
    /// Total txs observed leaving the pool (mined or dropped) since start.
    pub departed_total: u64,
    /// Number of txs currently tracked (== current pool size as last seen).
    pub tracked: usize,
}

impl ChainTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a snapshot: rewrite `first_seen_local` on txs seen in earlier
    /// snapshots, record newly appeared txs (at `snapshot.taken_at`), prune
    /// and count txs that disappeared, and feed the arrival windows.
    ///
    /// Must be called with snapshots in chronological order.
    pub fn apply(&mut self, snapshot: &mut MempoolSnapshot) {
        let _ = snapshot;
        todo!("implemented by tracker owner (see task contract)")
    }

    /// Derived dynamics as of `now` (usually the latest snapshot time or
    /// SystemTime::now()).
    pub fn stats(&self, now: SystemTime) -> TrackerStats {
        let _ = now;
        todo!("implemented by tracker owner (see task contract)")
    }
}
