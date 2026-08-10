//! Percentiles, fee bands, and fixed-capacity time series for sparklines.

use std::collections::VecDeque;
use std::time::SystemTime;

use crate::model::{FeeBand, PoolTx};

/// Percentile labels/values, in order, used by [`fee_bands`].
const FEE_BAND_LEVELS: [(&str, f64); 5] = [
    ("p10", 10.0),
    ("p25", 25.0),
    ("p50", 50.0),
    ("p75", 75.0),
    ("p90", 90.0),
];

/// Nearest-rank percentile over an ascending-sorted slice.
/// `p` in [0.0, 100.0]. Returns None for an empty slice.
pub fn percentile(sorted_asc: &[f64], p: f64) -> Option<f64> {
    if sorted_asc.is_empty() {
        None
    } else {
        Some(nearest_rank_value(sorted_asc, p))
    }
}

/// Nearest-rank value for `p` in a non-empty ascending-sorted slice.
///
/// rank = ceil(p/100 * n), clamped to [1, n]; returns the element at
/// rank-1. `p <= 0` clamps to rank 1 (first element); `p >= 100` clamps to
/// rank n (last element). `p` is clamped to [0.0, 100.0] before use, so a
/// non-finite `p` (e.g. NaN, which `f64::clamp` passes through unchanged)
/// can only ever produce a rank that `usize` saturating-casts to 0, which
/// the final `.clamp(1, n)` brings back into range -- this never panics.
fn nearest_rank_value(sorted_asc: &[f64], p: f64) -> f64 {
    debug_assert!(!sorted_asc.is_empty());
    let n = sorted_asc.len();
    let clamped_p = p.clamp(0.0, 100.0);
    let rank = ((clamped_p / 100.0) * n as f64).ceil() as usize;
    let rank = rank.clamp(1, n);
    sorted_asc[rank - 1]
}

/// Fee-per-byte percentile bands p10/p25/p50/p75/p90 for one snapshot.
///
/// - `fee_per_byte` of each band is the nearest-rank percentile value.
/// - `tx_count` is the number of txs in the interval that ends at this
///   percentile (first band counts everything at or below p10).
/// - Empty input returns an empty Vec.
pub fn fee_bands(txs: &[PoolTx]) -> Vec<FeeBand> {
    if txs.is_empty() {
        return Vec::new();
    }

    let mut fees: Vec<f64> = txs.iter().map(|tx| tx.fee_per_byte).collect();
    fees.sort_by(f64::total_cmp);

    let mut bands = Vec::with_capacity(FEE_BAND_LEVELS.len());
    let mut prev_value: Option<f64> = None;

    for (label, p) in FEE_BAND_LEVELS {
        let value = nearest_rank_value(&fees, p);
        let tx_count = match prev_value {
            None => fees.iter().filter(|&&f| f <= value).count(),
            Some(prev) => fees.iter().filter(|&&f| f > prev && f <= value).count(),
        };

        bands.push(FeeBand {
            label: label.to_string(),
            fee_per_byte: value,
            tx_count,
        });
        prev_value = Some(value);
    }

    bands
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
        self.points.push_back((at, value));
        if self.points.len() > self.cap {
            self.points.pop_front();
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &(SystemTime, f64)> {
        self.points.iter()
    }

    pub fn latest(&self) -> Option<(SystemTime, f64)> {
        self.points.back().copied()
    }

    pub fn max_value(&self) -> Option<f64> {
        self.points
            .iter()
            .map(|&(_, v)| v)
            .filter(|v| v.is_finite())
            .fold(None, |acc: Option<f64>, v| match acc {
                None => Some(v),
                Some(m) => Some(m.max(v)),
            })
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::InstantTxKind;
    use std::time::Duration;

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn tx_with_fee(fee_per_byte: f64) -> PoolTx {
        PoolTx {
            txid: "t".to_string(),
            size_bytes: 250,
            fee_atomic: 0,
            fee_per_byte,
            received_by_node: None,
            first_seen_local: SystemTime::UNIX_EPOCH,
            instant: InstantTxKind::None,
            in_out_counts: None,
            double_spend_seen: false,
        }
    }

    // --- percentile ---

    #[test]
    fn percentile_empty_slice_is_none() {
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn percentile_1_to_10_known_values() {
        let v: Vec<f64> = (1..=10).map(|n| n as f64).collect();
        assert_eq!(percentile(&v, 50.0), Some(5.0));
        assert_eq!(percentile(&v, 90.0), Some(9.0));
        assert_eq!(percentile(&v, 10.0), Some(1.0));
        assert_eq!(percentile(&v, 0.0), Some(1.0));
        assert_eq!(percentile(&v, 100.0), Some(10.0));
    }

    #[test]
    fn percentile_single_element_always_returns_it() {
        let v = [42.0];
        assert_eq!(percentile(&v, 0.0), Some(42.0));
        assert_eq!(percentile(&v, 50.0), Some(42.0));
        assert_eq!(percentile(&v, 100.0), Some(42.0));
    }

    #[test]
    fn percentile_clamps_out_of_range_p() {
        let v: Vec<f64> = (1..=10).map(|n| n as f64).collect();
        assert_eq!(percentile(&v, -10.0), Some(1.0));
        assert_eq!(percentile(&v, 1000.0), Some(10.0));
    }

    #[test]
    fn percentile_never_panics_on_non_finite_p() {
        let v: Vec<f64> = (1..=10).map(|n| n as f64).collect();
        assert_eq!(percentile(&v, f64::NAN), Some(1.0));
        assert_eq!(percentile(&v, f64::INFINITY), Some(10.0));
        assert_eq!(percentile(&v, f64::NEG_INFINITY), Some(1.0));
    }

    #[test]
    fn percentile_monotonic_pseudo_random_sweep() {
        struct Lcg(u64);
        impl Lcg {
            fn next_u64(&mut self) -> u64 {
                // Numerical-Recipes-style LCG constants; deterministic,
                // no external dependency.
                self.0 = self
                    .0
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                self.0
            }
            fn next_f64(&mut self) -> f64 {
                (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
            }
        }

        for &size in &[1usize, 2, 3, 5, 17, 100] {
            let mut rng = Lcg(0x2545_F491_4F6C_DD1D ^ size as u64);
            let mut values: Vec<f64> = (0..size).map(|_| rng.next_f64() * 1000.0).collect();
            values.sort_by(f64::total_cmp);

            let mut prev: Option<f64> = None;
            for step in 0..=200u32 {
                let p = f64::from(step) * 0.5;
                let value = percentile(&values, p).expect("non-empty slice");
                if let Some(prev_value) = prev {
                    assert!(
                        value >= prev_value,
                        "percentile not monotonic at size={size} p={p}: {value} < {prev_value}"
                    );
                }
                prev = Some(value);
            }
        }
    }

    // --- fee_bands ---

    #[test]
    fn fee_bands_empty_input_is_empty() {
        assert_eq!(fee_bands(&[]), Vec::new());
    }

    #[test]
    fn fee_bands_known_distribution_exact_values_and_counts() {
        let txs: Vec<PoolTx> = (1..=8).map(|n| tx_with_fee(n as f64)).collect();
        let bands = fee_bands(&txs);

        let expected = [
            ("p10", 1.0, 1usize),
            ("p25", 2.0, 1),
            ("p50", 4.0, 2),
            ("p75", 6.0, 2),
            ("p90", 8.0, 2),
        ];

        assert_eq!(bands.len(), expected.len());
        for (band, (label, value, count)) in bands.iter().zip(expected.iter()) {
            assert_eq!(&band.label, label);
            assert_eq!(band.fee_per_byte, *value);
            assert_eq!(band.tx_count, *count);
        }

        // n=8 (<=9) means the p90 rank reaches n, so these bands fully
        // partition the data and counts sum to n exactly.
        let total: usize = bands.iter().map(|b| b.tx_count).sum();
        assert_eq!(total, txs.len());
    }

    #[test]
    fn fee_bands_top_decile_tail_is_excluded_from_all_bands() {
        // With n=10 and no ties, ceil(0.9*10)=9, so the p90 band's value is
        // the 9th-smallest element; the 10th (largest) element is strictly
        // greater than every band boundary and therefore not counted by
        // any band. This is intentional: bands mark "at or below this
        // percentile", not a full partition of the data.
        let txs: Vec<PoolTx> = (1..=10).map(|n| tx_with_fee(n as f64)).collect();
        let bands = fee_bands(&txs);
        let total: usize = bands.iter().map(|b| b.tx_count).sum();
        assert_eq!(total, 9);
        assert_eq!(bands.last().unwrap().fee_per_byte, 9.0);
    }

    #[test]
    fn fee_bands_all_equal_fees_first_band_carries_everything() {
        let txs: Vec<PoolTx> = (0..6).map(|_| tx_with_fee(7.5)).collect();
        let bands = fee_bands(&txs);

        assert_eq!(bands.len(), 5);
        assert_eq!(bands[0].tx_count, 6);
        assert_eq!(bands[0].fee_per_byte, 7.5);
        for band in &bands[1..] {
            assert_eq!(band.tx_count, 0);
            assert_eq!(band.fee_per_byte, 7.5);
        }
    }

    // --- TimeSeries ---

    #[test]
    fn time_series_push_and_iterate() {
        let mut ts = TimeSeries::new(3);
        ts.push(t(0), 1.0);
        ts.push(t(1), 2.0);
        let points: Vec<_> = ts.iter().cloned().collect();
        assert_eq!(points, vec![(t(0), 1.0), (t(1), 2.0)]);
        assert_eq!(ts.len(), 2);
        assert!(!ts.is_empty());
    }

    #[test]
    fn time_series_evicts_oldest_beyond_capacity() {
        let mut ts = TimeSeries::new(2);
        ts.push(t(0), 1.0);
        ts.push(t(1), 2.0);
        ts.push(t(2), 3.0);

        assert_eq!(ts.len(), 2);
        let points: Vec<_> = ts.iter().cloned().collect();
        assert_eq!(points, vec![(t(1), 2.0), (t(2), 3.0)]);
        assert_eq!(ts.latest(), Some((t(2), 3.0)));
    }

    #[test]
    fn time_series_max_value() {
        let mut ts = TimeSeries::new(5);
        assert_eq!(ts.max_value(), None);

        ts.push(t(0), 3.0);
        ts.push(t(1), 7.0);
        ts.push(t(2), 2.0);
        assert_eq!(ts.max_value(), Some(7.0));
    }

    #[test]
    fn time_series_max_value_ignores_non_finite() {
        let mut ts = TimeSeries::new(5);
        ts.push(t(0), 3.0);
        ts.push(t(1), f64::NAN);
        ts.push(t(2), f64::INFINITY);
        ts.push(t(3), 5.0);
        assert_eq!(ts.max_value(), Some(5.0));
    }

    #[test]
    fn time_series_empty() {
        let ts = TimeSeries::new(4);
        assert!(ts.is_empty());
        assert_eq!(ts.len(), 0);
        assert_eq!(ts.latest(), None);
        assert_eq!(ts.max_value(), None);
    }

    #[test]
    fn time_series_new_clamps_zero_capacity_to_one() {
        let mut ts = TimeSeries::new(0);
        ts.push(t(0), 1.0);
        ts.push(t(1), 2.0);
        assert_eq!(ts.len(), 1);
        assert_eq!(ts.latest(), Some((t(1), 2.0)));
    }
}
