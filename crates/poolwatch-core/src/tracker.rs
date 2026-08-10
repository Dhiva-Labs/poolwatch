//! First-seen tracking and pool-dynamics derivation.
//!
//! The tracker maintains a per-chain map of txid -> first-seen timestamp,
//! pruned when txs leave the pool (mined or dropped). From consecutive
//! snapshots it derives what daemons don't expose: inflow rate (tx/min over
//! rolling 1m/5m/15m windows), median residency of the current pool, and a
//! count of departed txs.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, SystemTime};

use crate::model::MempoolSnapshot;

/// Width of the inflow windows and the horizon beyond which arrival events
/// are pruned (the widest window, so shorter windows are always exact).
const INFLOW_WINDOW_1M: Duration = Duration::from_secs(60);
const INFLOW_WINDOW_5M: Duration = Duration::from_secs(5 * 60);
const INFLOW_WINDOW_15M: Duration = Duration::from_secs(15 * 60);

/// Rolling per-chain state. One instance per configured chain.
#[derive(Debug, Default)]
pub struct ChainTracker {
    /// txid -> first-seen timestamp, for every tx currently tracked (i.e.
    /// present in the most recently applied snapshot).
    first_seen: HashMap<String, SystemTime>,
    /// Arrival event times, oldest first (snapshots are applied in
    /// chronological order, so this stays sorted by construction). Pruned
    /// to the widest inflow window on every `apply`.
    arrivals: VecDeque<SystemTime>,
    /// Cumulative count of txs observed leaving the pool (mined or
    /// dropped), across the tracker's whole lifetime.
    departed_total: u64,
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
        let now = snapshot.taken_at;
        let mut present: HashSet<String> = HashSet::with_capacity(snapshot.txs.len());

        for tx in snapshot.txs.iter_mut() {
            present.insert(tx.txid.clone());

            match self.first_seen.entry(tx.txid.clone()) {
                Entry::Occupied(entry) => {
                    // Already tracked: seen in an earlier snapshot, or an
                    // earlier tx in this same snapshot sharing the txid
                    // (duplicate). Either way it's not a new arrival.
                    tx.first_seen_local = *entry.get();
                }
                Entry::Vacant(entry) => {
                    // Brand-new txid: first-seen is the snapshot time, not
                    // wall-clock "now" -- keeps replays/backfills and tests
                    // deterministic.
                    tx.first_seen_local = now;
                    entry.insert(now);
                    self.arrivals.push_back(now);
                }
            }
        }

        // Anything previously tracked but absent from this snapshot has
        // left the pool (mined or dropped). A txid that reappears in a
        // later snapshot is not in `first_seen` anymore at that point, so
        // it is treated as a fresh arrival, per contract.
        let before = self.first_seen.len();
        self.first_seen.retain(|txid, _| present.contains(txid));
        self.departed_total += (before - self.first_seen.len()) as u64;

        prune_arrivals(&mut self.arrivals, now);
    }

    /// Derived dynamics as of `now` (usually the latest snapshot time or
    /// SystemTime::now()).
    pub fn stats(&self, now: SystemTime) -> TrackerStats {
        let arrivals_1m = self.count_arrivals_within(now, INFLOW_WINDOW_1M);
        let arrivals_5m = self.count_arrivals_within(now, INFLOW_WINDOW_5M);
        let arrivals_15m = self.count_arrivals_within(now, INFLOW_WINDOW_15M);

        TrackerStats {
            inflow_1m: arrivals_1m as f64 / 1.0,
            inflow_5m: arrivals_5m as f64 / 5.0,
            inflow_15m: arrivals_15m as f64 / 15.0,
            median_residency: self.median_residency(now),
            departed_total: self.departed_total,
            tracked: self.first_seen.len(),
        }
    }

    /// Number of recorded arrival events whose event time is within
    /// `window` of `now` (inclusive).
    fn count_arrivals_within(&self, now: SystemTime, window: Duration) -> usize {
        // `arrivals` is sorted oldest-first; walk from the newest end and
        // stop at the first entry that falls outside the window.
        self.arrivals
            .iter()
            .rev()
            .take_while(|&&at| elapsed(at, now) <= window)
            .count()
    }

    /// Median age of currently-tracked txs, or None if nothing is tracked.
    ///
    /// Tie-break for an even-sized pool: of the two middle ages, the
    /// *lower* (smaller / more recent-arrival... i.e. numerically smaller
    /// duration) one is returned, rather than interpolating between them.
    fn median_residency(&self, now: SystemTime) -> Option<Duration> {
        if self.first_seen.is_empty() {
            return None;
        }

        let mut ages: Vec<Duration> = self
            .first_seen
            .values()
            .map(|&first_seen| elapsed(first_seen, now))
            .collect();
        ages.sort_unstable();

        // For n elements (n >= 1), (n-1)/2 is the exact middle for odd n
        // and the lower of the two middle indices for even n.
        let mid = (ages.len() - 1) / 2;
        Some(ages[mid])
    }
}

/// Drop arrival events older than the widest inflow window, bounding the
/// deque's memory use. Called at the end of every `apply`.
fn prune_arrivals(arrivals: &mut VecDeque<SystemTime>, now: SystemTime) {
    while let Some(&oldest) = arrivals.front() {
        if elapsed(oldest, now) > INFLOW_WINDOW_15M {
            arrivals.pop_front();
        } else {
            break;
        }
    }
}

/// `later.duration_since(earlier)`, treating any failure (system clock
/// skew making `earlier` appear after `later`) as zero elapsed time rather
/// than panicking.
fn elapsed(earlier: SystemTime, later: SystemTime) -> Duration {
    later.duration_since(earlier).unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstantTxKind, PoolTx};

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn tx(txid: &str) -> PoolTx {
        PoolTx {
            txid: txid.to_string(),
            size_bytes: 250,
            fee_atomic: 1_000,
            fee_per_byte: 4.0,
            received_by_node: None,
            first_seen_local: SystemTime::UNIX_EPOCH, // overwritten by apply()
            instant: InstantTxKind::None,
            in_out_counts: None,
            double_spend_seen: false,
        }
    }

    fn snapshot(taken_at: SystemTime, txids: &[&str]) -> MempoolSnapshot {
        MempoolSnapshot {
            chain_id: "test-chain".to_string(),
            taken_at,
            txs: txids.iter().map(|id| tx(id)).collect(),
            pool_bytes: 0,
            node_height: 0,
            node_version: "test".to_string(),
            peer_count: None,
        }
    }

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn apply_rewrites_first_seen_across_snapshots() {
        let mut tracker = ChainTracker::new();

        let mut snap1 = snapshot(t(0), &["a"]);
        tracker.apply(&mut snap1);
        assert_eq!(snap1.txs[0].first_seen_local, t(0));

        // Simulate an adapter setting first_seen_local to its own fetch
        // time; apply() must rewrite it back to the true first-seen (t(0)).
        let mut snap2 = snapshot(t(30), &["a"]);
        snap2.txs[0].first_seen_local = t(30);
        tracker.apply(&mut snap2);
        assert_eq!(snap2.txs[0].first_seen_local, t(0));

        let stats = tracker.stats(t(30));
        assert_eq!(stats.tracked, 1);
        assert_eq!(stats.departed_total, 0);
    }

    #[test]
    fn inflow_windows_count_arrivals_within_each_window() {
        let mut tracker = ChainTracker::new();
        let ids = ["a", "b", "c", "d", "e", "f"];
        let offsets = [0u64, 15, 30, 45, 60, 75];

        for i in 0..ids.len() {
            let mut snap = snapshot(t(offsets[i]), &ids[..=i]);
            tracker.apply(&mut snap);
        }

        // now = 90: arrival diffs are 90,75,60,45,30,15. Within the last
        // 60s (diff <= 60): the last four (60,45,30,15) => 4 arrivals.
        let stats = tracker.stats(t(90));
        assert!(
            approx_eq(stats.inflow_1m, 4.0),
            "inflow_1m = {}",
            stats.inflow_1m
        );
        // All 6 arrivals fall within 5 and 15 minutes of now.
        assert!(
            approx_eq(stats.inflow_5m, 6.0 / 5.0),
            "inflow_5m = {}",
            stats.inflow_5m
        );
        assert!(
            approx_eq(stats.inflow_15m, 6.0 / 15.0),
            "inflow_15m = {}",
            stats.inflow_15m
        );
        assert_eq!(stats.tracked, 6);
        assert_eq!(stats.departed_total, 0);
    }

    #[test]
    fn departed_txs_are_counted_and_removed() {
        let mut tracker = ChainTracker::new();
        let mut snap1 = snapshot(t(0), &["a", "b"]);
        tracker.apply(&mut snap1);

        let mut snap2 = snapshot(t(60), &["a"]); // "b" left the pool
        tracker.apply(&mut snap2);

        let stats = tracker.stats(t(60));
        assert_eq!(stats.departed_total, 1);
        assert_eq!(stats.tracked, 1);
    }

    #[test]
    fn reappearing_txid_counts_as_fresh_arrival() {
        let mut tracker = ChainTracker::new();

        let mut snap1 = snapshot(t(0), &["a"]);
        tracker.apply(&mut snap1);

        let mut snap2 = snapshot(t(60), &[]); // "a" departs
        tracker.apply(&mut snap2);
        assert_eq!(tracker.stats(t(60)).departed_total, 1);
        assert_eq!(tracker.stats(t(60)).tracked, 0);

        let mut snap3 = snapshot(t(120), &["a"]); // "a" reappears
        tracker.apply(&mut snap3);
        assert_eq!(snap3.txs[0].first_seen_local, t(120)); // fresh, not t(0)

        let stats = tracker.stats(t(120));
        assert_eq!(stats.tracked, 1);
        assert_eq!(stats.departed_total, 1); // not incremented again

        // Two distinct arrival events (t(0) and t(120)) both fall within
        // the 15-minute window measured from now = t(120).
        assert!(approx_eq(stats.inflow_15m, 2.0 / 15.0));
    }

    #[test]
    fn duplicate_txid_within_snapshot_counts_once() {
        let mut tracker = ChainTracker::new();
        let mut snap = snapshot(t(0), &["dup", "dup"]);
        tracker.apply(&mut snap);

        assert_eq!(snap.txs[0].first_seen_local, t(0));
        assert_eq!(snap.txs[1].first_seen_local, t(0));

        let stats = tracker.stats(t(0));
        assert_eq!(stats.tracked, 1);
        assert!(approx_eq(stats.inflow_1m, 1.0));
    }

    #[test]
    fn median_residency_even_count_picks_lower_middle() {
        let mut tracker = ChainTracker::new();
        let ids = ["a", "b", "c", "d"];
        for i in 0..ids.len() {
            let mut snap = snapshot(t(i as u64 * 10), &ids[..=i]);
            tracker.apply(&mut snap);
        }

        // Arrivals: a@0, b@10, c@20, d@30. At now=30, ages are
        // a=30, b=20, c=10, d=0 -> sorted [0,10,20,30].
        // Lower-middle index = (4-1)/2 = 1 -> 10s.
        let stats = tracker.stats(t(30));
        assert_eq!(stats.median_residency, Some(Duration::from_secs(10)));
    }

    #[test]
    fn median_residency_odd_count_is_exact_middle() {
        let mut tracker = ChainTracker::new();
        let ids = ["a", "b", "c", "d", "e"];
        for i in 0..ids.len() {
            let mut snap = snapshot(t(i as u64 * 10), &ids[..=i]);
            tracker.apply(&mut snap);
        }

        // At now=40, ages are a=40,b=30,c=20,d=10,e=0 -> sorted
        // [0,10,20,30,40]. Odd count -> exact middle index (5-1)/2=2 -> 20s.
        let stats = tracker.stats(t(40));
        assert_eq!(stats.median_residency, Some(Duration::from_secs(20)));
    }

    #[test]
    fn empty_tracker_has_default_stats() {
        let tracker = ChainTracker::new();
        let stats = tracker.stats(t(0));
        assert_eq!(stats.tracked, 0);
        assert_eq!(stats.departed_total, 0);
        assert_eq!(stats.median_residency, None);
        assert_eq!(stats.inflow_1m, 0.0);
        assert_eq!(stats.inflow_5m, 0.0);
        assert_eq!(stats.inflow_15m, 0.0);
    }

    #[test]
    fn pool_empties_out_after_all_txs_depart() {
        let mut tracker = ChainTracker::new();
        let mut snap1 = snapshot(t(0), &["a", "b"]);
        tracker.apply(&mut snap1);
        let mut snap2 = snapshot(t(10), &[]);
        tracker.apply(&mut snap2);

        let stats = tracker.stats(t(10));
        assert_eq!(stats.tracked, 0);
        assert_eq!(stats.departed_total, 2);
        assert_eq!(stats.median_residency, None);
    }

    #[test]
    fn clock_skew_is_treated_as_zero_not_panic() {
        let mut tracker = ChainTracker::new();
        let mut snap = snapshot(t(1_000), &["a"]);
        tracker.apply(&mut snap);

        // `now` earlier than the recorded first-seen time (simulated clock
        // skew). Must not panic; residency clamps to zero.
        let stats = tracker.stats(t(0));
        assert_eq!(stats.median_residency, Some(Duration::ZERO));
        // duration_since fails (now < first_seen) so elapsed clamps to
        // zero, meaning the arrival reads as within every inflow window.
        assert!(approx_eq(stats.inflow_1m, 1.0));
        assert!(approx_eq(stats.inflow_15m, 1.0 / 15.0));
    }

    #[test]
    fn arrivals_older_than_15_minutes_are_pruned_on_apply() {
        let mut tracker = ChainTracker::new();
        let mut snap1 = snapshot(t(0), &["a"]);
        tracker.apply(&mut snap1);
        assert_eq!(tracker.arrivals.len(), 1);

        // Advance 20 minutes (past the 15-minute horizon) and add "b" so
        // "a" stays tracked (present) while its old arrival event ages out.
        let mut snap2 = snapshot(t(20 * 60), &["a", "b"]);
        tracker.apply(&mut snap2);

        // "a"'s stale arrival event was pruned; "b"'s fresh one remains.
        assert_eq!(tracker.arrivals.len(), 1);
        assert_eq!(tracker.stats(t(20 * 60)).tracked, 2);
        assert_eq!(tracker.stats(t(20 * 60)).departed_total, 0);
    }
}
