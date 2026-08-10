//! Percentiles, fee bands, and fixed-capacity time series for sparklines.

use std::collections::VecDeque;
use std::time::SystemTime;

use crate::model::{FeeBand, PoolTx};

/// Nearest-rank percentile over an ascending-sorted slice.
/// `p` in [0.0, 100.0]. Returns None for an empty slice.
pub fn percentile(sorted_asc: &[f64], p: f64) -> Option<f64> {
    let _ = (sorted_asc, p);
    todo!("implemented by stats owner (see task contract)")
}

/// Fee-per-byte percentile bands p10/p25/p50/p75/p90 for one snapshot.
///
/// - `fee_per_byte` of each band is the nearest-rank percentile value.
/// - `tx_count` is the number of txs in the interval that ends at this
///   percentile (first band counts everything at or below p10).
/// - Empty input returns an empty Vec.
pub fn fee_bands(txs: &[PoolTx]) -> Vec<FeeBand> {
    let _ = txs;
    todo!("implemented by stats owner (see task contract)")
}

/// Fixed-capacity time series (ring buffer) for sparklines and rolling
/// windows. Pushing beyond capacity drops the oldest point.
#[derive(Debug, Clone)]
pub struct TimeSeries {
    points: VecDeque<(SystemTime, f64)>,
    cap: usize,
}

impl TimeSeries {
    /// `cap` must be > 0.
    pub fn new(cap: usize) -> Self {
        Self {
            points: VecDeque::with_capacity(cap),
            cap: cap.max(1),
        }
    }

    pub fn push(&mut self, at: SystemTime, value: f64) {
        let _ = (at, value);
        todo!("implemented by stats owner (see task contract)")
    }

    pub fn iter(&self) -> impl Iterator<Item = &(SystemTime, f64)> {
        self.points.iter()
    }

    pub fn latest(&self) -> Option<(SystemTime, f64)> {
        self.points.back().copied()
    }

    pub fn max_value(&self) -> Option<f64> {
        let _ = &self.cap;
        todo!("implemented by stats owner (see task contract)")
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}
