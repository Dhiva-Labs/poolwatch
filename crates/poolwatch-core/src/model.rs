//! Normalized, chain-agnostic domain types.
//!
//! Contract rules:
//! - Amounts are always atomic units (piconero, satoshi, ...) in `u128`.
//! - `fee_per_byte` is atomic units per byte (per *vbyte* for bitcoind
//!   family, which reports `vsize`).
//! - Formatting into human units happens only in the UI layer, using
//!   [`ChainMeta::decimals`] and [`ChainMeta::fee_rate_unit`].

use std::time::SystemTime;

/// Normalized snapshot — everything the UI needs, chain-agnostic.
#[derive(Debug, Clone)]
pub struct MempoolSnapshot {
    /// Config key, e.g. "monero-main".
    pub chain_id: String,
    pub taken_at: SystemTime,
    pub txs: Vec<PoolTx>,
    /// Total pool size in bytes as reported by the daemon (sum of tx sizes
    /// if the daemon does not report a total).
    pub pool_bytes: u64,
    pub node_height: u64,
    pub node_version: String,
    pub peer_count: Option<u32>,
}

/// One transaction currently in the pool.
#[derive(Debug, Clone)]
pub struct PoolTx {
    pub txid: String,
    /// Blob size for CryptoNote chains; vsize for bitcoind family.
    pub size_bytes: u64,
    /// Fee in atomic units (piconero, satoshi, ...).
    pub fee_atomic: u128,
    /// Derived: atomic units per (v)byte.
    pub fee_per_byte: f64,
    /// Daemon-reported receive time, if available and non-zero.
    pub received_by_node: Option<SystemTime>,
    /// poolwatch's own first-seen timestamp. Adapters set this to "now" at
    /// fetch time; [`crate::tracker::ChainTracker::apply`] rewrites it to the
    /// true first observation for txs seen in earlier snapshots.
    pub first_seen_local: SystemTime,
    pub instant: InstantTxKind,
    /// CryptoNote: (ring inputs, outputs) parsed from tx_json.
    pub in_out_counts: Option<(u16, u16)>,
    /// CryptoNote double_spend_seen flag.
    pub double_spend_seen: bool,
}

/// Instant-transaction classification (Oxen Blink / Beldex Flash).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstantTxKind {
    #[default]
    None,
    Blink,
    Flash,
}

/// One fee-per-byte percentile band (p10/p25/p50/p75/p90).
#[derive(Debug, Clone, PartialEq)]
pub struct FeeBand {
    /// "p10", "p25", "p50", "p75", "p90".
    pub label: String,
    /// Fee rate at this percentile, atomic units per (v)byte.
    pub fee_per_byte: f64,
    /// Number of pool txs falling in the interval that ends at this
    /// percentile (the first band counts everything at or below p10).
    pub tx_count: usize,
}

/// Cheap high-frequency pool stats (no per-tx data).
#[derive(Debug, Clone)]
pub struct PoolStats {
    pub chain_id: String,
    pub taken_at: SystemTime,
    pub tx_count: u64,
    pub pool_bytes: u64,
    /// Total fees currently in the pool, atomic units, if reported.
    pub fee_total: Option<u128>,
    /// CryptoNote: number of double spends seen, if reported.
    pub double_spends: Option<u64>,
    /// Timestamp of the oldest tx in the pool, if reported.
    pub oldest: Option<SystemTime>,
}

/// Static per-chain metadata carried by every adapter.
#[derive(Debug, Clone)]
pub struct ChainMeta {
    /// Config key, e.g. "monero-main". Unique across the app.
    pub id: String,
    /// Human name for the UI, e.g. "Monero".
    pub display_name: String,
    /// Ticker, e.g. "XMR".
    pub ticker: String,
    /// Number of decimal places between the atomic unit and the main unit.
    pub decimals: u32,
    /// Display label for fee rates, e.g. "pN/B", "sat/vB".
    pub fee_rate_unit: String,
}
