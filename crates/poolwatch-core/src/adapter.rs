//! The adapter contract. One implementation per RPC family
//! (cryptonote: monerod/oxend/beldexd; bitcoind: bitcoind and forks).

use async_trait::async_trait;

use crate::model::{ChainMeta, MempoolSnapshot, PoolStats};

/// Errors an adapter can surface. The poller maps these onto per-chain
/// status (degraded / unreachable) — they must never panic the app.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// Transport-level failure (connection refused, timeout, TLS, ...).
    #[error("http: {0}")]
    Http(String),

    /// The RPC endpoint answered with an error object.
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },

    /// Authentication failed (digest, basic, cookie).
    #[error("auth: {0}")]
    Auth(String),

    /// Response exceeded the configured size guard; caller should degrade
    /// to stats-only mode for this chain.
    #[error("response too large: {size} bytes (limit {limit})")]
    TooLarge { size: u64, limit: u64 },

    /// Response arrived but could not be parsed into the domain model.
    #[error("parse: {0}")]
    Parse(String),

    /// Endpoint not supported by this daemon (HTTP 404/405) — treat as a
    /// capability flag, disable the feature for the chain, don't crash.
    #[error("unsupported endpoint: {0}")]
    Unsupported(String),

    /// Local I/O failure (e.g. reading a cookie file).
    #[error("io: {0}")]
    Io(String),
}

/// The adapter contract. One impl per RPC family.
#[async_trait]
pub trait ChainAdapter: Send + Sync {
    /// Full pool fetch (potentially large). Called at `poll_interval_secs`.
    async fn fetch_snapshot(&self) -> Result<MempoolSnapshot, AdapterError>;

    /// Cheap call for high-frequency polling (pool count/bytes only).
    /// Called at `stats_interval_secs`.
    async fn fetch_pool_stats(&self) -> Result<PoolStats, AdapterError>;

    /// Static metadata (ticker, decimals, display name).
    fn chain_meta(&self) -> &ChainMeta;
}
