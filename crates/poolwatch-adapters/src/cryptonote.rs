//! CryptoNote-family adapter: monerod, oxend, beldexd and compatible forks.

use async_trait::async_trait;
use poolwatch_core::{
    AdapterError, ChainAdapter, ChainMeta, InstantTxKind, MempoolSnapshot, PoolStats,
};

/// Constructor config for [`CryptonoteAdapter`]. Built by the TUI layer
/// from a `[[chain]]` block with `family = "cryptonote"`.
#[derive(Debug, Clone)]
pub struct CryptonoteConfig {
    pub meta: ChainMeta,
    /// e.g. "http://127.0.0.1:18081" (no trailing slash required).
    pub rpc_url: String,
    /// monerod --rpc-login "user:pass" (HTTP digest auth), if set.
    pub rpc_login: Option<String>,
    /// Some(Blink) for Oxen, Some(Flash) for Beldex, None otherwise.
    /// Enables instant-tx detection for the chain.
    pub instant_tx: Option<InstantTxKind>,
    /// Size guard for /get_transaction_pool responses; exceeding it yields
    /// AdapterError::TooLarge so the poller can degrade to stats-only mode.
    pub max_pool_response_bytes: u64,
}

pub struct CryptonoteAdapter {
    config: CryptonoteConfig,
}

impl CryptonoteAdapter {
    pub fn new(config: CryptonoteConfig) -> Result<Self, AdapterError> {
        Ok(Self { config })
    }
}

#[async_trait]
impl ChainAdapter for CryptonoteAdapter {
    async fn fetch_snapshot(&self) -> Result<MempoolSnapshot, AdapterError> {
        todo!("implemented by adapters owner (see task contract)")
    }

    async fn fetch_pool_stats(&self) -> Result<PoolStats, AdapterError> {
        todo!("implemented by adapters owner (see task contract)")
    }

    fn chain_meta(&self) -> &ChainMeta {
        &self.config.meta
    }
}
