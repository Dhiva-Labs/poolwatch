//! bitcoind-family adapter: bitcoind and compatible forks (JSON-RPC 1.0).

use std::path::PathBuf;

use async_trait::async_trait;
use poolwatch_core::{AdapterError, ChainAdapter, ChainMeta, MempoolSnapshot, PoolStats};

/// Auth for the bitcoind RPC. Exactly one variant per chain config.
#[derive(Debug, Clone)]
pub enum BitcoindAuth {
    /// Path to the `.cookie` file (`__cookie__:token`); re-read on 401
    /// because it rotates on node restart.
    CookieFile(PathBuf),
    UserPass {
        user: String,
        password: String,
    },
}

/// Constructor config for [`BitcoindAdapter`]. Built by the TUI layer from
/// a `[[chain]]` block with `family = "bitcoind"`.
#[derive(Debug, Clone)]
pub struct BitcoindConfig {
    pub meta: ChainMeta,
    /// e.g. "http://127.0.0.1:8332".
    pub rpc_url: String,
    pub auth: BitcoindAuth,
}

pub struct BitcoindAdapter {
    config: BitcoindConfig,
}

impl BitcoindAdapter {
    pub fn new(config: BitcoindConfig) -> Result<Self, AdapterError> {
        Ok(Self { config })
    }
}

#[async_trait]
impl ChainAdapter for BitcoindAdapter {
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
