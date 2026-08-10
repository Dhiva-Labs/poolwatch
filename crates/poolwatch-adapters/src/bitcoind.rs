//! bitcoind-family adapter: bitcoind and compatible forks (JSON-RPC 1.0).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use poolwatch_core::{
    AdapterError, ChainAdapter, ChainMeta, InstantTxKind, MempoolSnapshot, PoolStats, PoolTx,
};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::rpc::{
    jsonrpc_result, Auth, BasicAuth, HttpClient, NodeInfo, ParsedPool, SMALL_RESPONSE_LIMIT,
};

/// Bitcoin amounts always carry 8 decimal places.
const BTC_DECIMALS: usize = 8;
const SATS_PER_BTC: u128 = 100_000_000;

/// [`BitcoindConfig`] is a frozen constructor API with no size-guard field,
/// so the full-pool guard is a constant here. It matches the documented
/// `max_pool_response_mb` default in poolwatch.example.toml.
const MAX_POOL_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;

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
    http: HttpClient,
}

impl BitcoindAdapter {
    pub fn new(config: BitcoindConfig) -> Result<Self, AdapterError> {
        let auth = Auth::Basic(match &config.auth {
            BitcoindAuth::CookieFile(path) => BasicAuth::cookie(path.clone()),
            BitcoindAuth::UserPass { user, password } => BasicAuth::UserPass {
                user: user.clone(),
                password: password.clone(),
            },
        });
        Ok(Self {
            config,
            http: HttpClient::new(auth)?,
        })
    }

    async fn call(&self, request: &str, limit: u64) -> Result<Vec<u8>, AdapterError> {
        self.http
            .post_json(&self.config.rpc_url, request.as_bytes(), limit)
            .await
    }
}

#[async_trait]
impl ChainAdapter for BitcoindAdapter {
    async fn fetch_snapshot(&self) -> Result<MempoolSnapshot, AdapterError> {
        let taken_at = SystemTime::now();

        let pool_body = self
            .call(
                r#"{"jsonrpc":"1.0","id":"poolwatch","method":"getrawmempool","params":[true]}"#,
                MAX_POOL_RESPONSE_BYTES,
            )
            .await?;
        let pool = parse_raw_mempool(&pool_body, taken_at)?;

        let chain_body = self
            .call(
                r#"{"jsonrpc":"1.0","id":"poolwatch","method":"getblockchaininfo","params":[]}"#,
                SMALL_RESPONSE_LIMIT,
            )
            .await?;
        let network_body = self
            .call(
                r#"{"jsonrpc":"1.0","id":"poolwatch","method":"getnetworkinfo","params":[]}"#,
                SMALL_RESPONSE_LIMIT,
            )
            .await?;
        let info = parse_node_info(&chain_body, &network_body)?;

        Ok(MempoolSnapshot {
            chain_id: self.config.meta.id.clone(),
            taken_at,
            txs: pool.txs,
            pool_bytes: pool.pool_bytes,
            node_height: info.height,
            node_version: info.version,
            peer_count: info.peer_count,
        })
    }

    async fn fetch_pool_stats(&self) -> Result<PoolStats, AdapterError> {
        let taken_at = SystemTime::now();
        let body = self
            .call(
                r#"{"jsonrpc":"1.0","id":"poolwatch","method":"getmempoolinfo","params":[]}"#,
                SMALL_RESPONSE_LIMIT,
            )
            .await?;
        parse_mempool_info(&body, &self.config.meta.id, taken_at)
    }

    fn chain_meta(&self) -> &ChainMeta {
        &self.config.meta
    }
}

// ---------------------------------------------------------------------------
// Wire types. Fee-bearing fields are kept as raw JSON text so the decimal is
// never routed through f64 — see `btc_decimal_to_atomic`.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct MempoolEntry {
    /// Virtual size; `size` on pre-0.19 nodes. This is what the fee rate is
    /// quoted against, so it is what we store in `PoolTx::size_bytes`.
    #[serde(default)]
    vsize: Option<u64>,
    #[serde(default)]
    size: Option<u64>,
    /// Unix seconds the node first accepted the tx.
    #[serde(default)]
    time: i64,
    #[serde(default)]
    fees: Option<MempoolEntryFees>,
    /// Top-level `fee` was removed in Core 22 but survives on forks.
    #[serde(default)]
    fee: Option<Box<RawValue>>,
}

#[derive(Deserialize)]
struct MempoolEntryFees {
    base: Box<RawValue>,
}

#[derive(Deserialize)]
struct MempoolInfo {
    #[serde(default)]
    size: u64,
    #[serde(default)]
    bytes: u64,
    /// Added in Core 23; absent on older nodes.
    #[serde(default)]
    total_fee: Option<Box<RawValue>>,
}

#[derive(Deserialize)]
struct BlockchainInfo {
    #[serde(default)]
    blocks: u64,
}

#[derive(Deserialize)]
struct NetworkInfo {
    /// "/Satoshi:28.0.0/".
    #[serde(default)]
    subversion: Option<String>,
    /// Numeric fallback, e.g. 280000.
    #[serde(default)]
    version: Option<u64>,
    #[serde(default)]
    connections: Option<u32>,
}

// ---------------------------------------------------------------------------
// Pure parsers (fixture-tested).
// ---------------------------------------------------------------------------

/// Convert a Bitcoin amount, as it appears verbatim in the JSON response, to
/// satoshis.
///
/// The conversion is done on the decimal *string*: `0.1_f64 * 1e8` and
/// friends do not round-trip exactly, and a mempool monitor that reports a
/// fee off by one satoshi is worse than useless. Core formats amounts with
/// `%d.%08d`, never with an exponent, so an `e` means we are not looking at
/// an amount we understand and must refuse rather than guess.
pub fn btc_decimal_to_atomic(raw: &str) -> Result<u128, AdapterError> {
    // Forks occasionally quote amounts as JSON strings; tolerate that.
    let text = raw.trim().trim_matches('"').trim();
    if text.is_empty() {
        return Err(amount_error(raw, "empty amount"));
    }
    if text.contains(['e', 'E']) {
        return Err(amount_error(raw, "exponent notation is not supported"));
    }

    let (whole_text, fraction_text) = text.split_once('.').unwrap_or((text, ""));
    if whole_text.is_empty() && fraction_text.is_empty() {
        return Err(amount_error(raw, "no digits"));
    }
    // Also rejects signs and a second '.'.
    if !whole_text.chars().all(|c| c.is_ascii_digit())
        || !fraction_text.chars().all(|c| c.is_ascii_digit())
    {
        return Err(amount_error(raw, "not a plain decimal number"));
    }
    if fraction_text.len() > BTC_DECIMALS {
        return Err(amount_error(
            raw,
            "more than 8 fractional digits (not representable in satoshis)",
        ));
    }

    let whole: u128 = if whole_text.is_empty() {
        0
    } else {
        whole_text
            .parse()
            .map_err(|_| amount_error(raw, "integer part out of range"))?
    };
    let fraction: u128 = if fraction_text.is_empty() {
        0
    } else {
        fraction_text
            .parse::<u128>()
            .map_err(|_| amount_error(raw, "fractional part out of range"))?
            // Right-pad to 8 digits: "0.1" is 10000000 sat, not 1.
            * 10u128.pow((BTC_DECIMALS - fraction_text.len()) as u32)
    };

    whole
        .checked_mul(SATS_PER_BTC)
        .and_then(|sats| sats.checked_add(fraction))
        .ok_or_else(|| amount_error(raw, "amount overflows u128 satoshis"))
}

fn amount_error(raw: &str, reason: &str) -> AdapterError {
    AdapterError::Parse(format!("bitcoin amount {raw:?}: {reason}"))
}

/// Parse a `getrawmempool true` response body.
pub fn parse_raw_mempool(body: &[u8], now: SystemTime) -> Result<ParsedPool, AdapterError> {
    // BTreeMap so the pool order is deterministic across polls.
    let entries: BTreeMap<String, MempoolEntry> = jsonrpc_result(body)?;

    let mut txs = Vec::with_capacity(entries.len());
    let mut pool_bytes: u64 = 0;

    for (txid, entry) in entries {
        let size_bytes = entry.vsize.or(entry.size).ok_or_else(|| {
            AdapterError::Parse(format!("mempool entry {txid} has neither vsize nor size"))
        })?;
        let fee_raw = entry
            .fees
            .as_ref()
            .map(|fees| fees.base.get())
            .or(entry.fee.as_deref().map(RawValue::get))
            .ok_or_else(|| AdapterError::Parse(format!("mempool entry {txid} has no fee")))?;
        let fee_atomic = btc_decimal_to_atomic(fee_raw)?;

        pool_bytes = pool_bytes.saturating_add(size_bytes);
        txs.push(PoolTx {
            txid,
            size_bytes,
            fee_atomic,
            fee_per_byte: fee_per_vbyte(fee_atomic, size_bytes),
            received_by_node: unix_time(entry.time),
            first_seen_local: now,
            // No bitcoind-family daemon has instant transactions.
            instant: InstantTxKind::None,
            in_out_counts: None,
            // A CryptoNote concept; bitcoind rejects conflicting txs outright.
            double_spend_seen: false,
        });
    }

    Ok(ParsedPool { txs, pool_bytes })
}

/// Parse a `getmempoolinfo` response body.
pub fn parse_mempool_info(
    body: &[u8],
    chain_id: &str,
    taken_at: SystemTime,
) -> Result<PoolStats, AdapterError> {
    let info: MempoolInfo = jsonrpc_result(body)?;
    let fee_total = match &info.total_fee {
        Some(raw) => Some(btc_decimal_to_atomic(raw.get())?),
        None => None,
    };

    Ok(PoolStats {
        chain_id: chain_id.to_string(),
        taken_at,
        tx_count: info.size,
        pool_bytes: info.bytes,
        fee_total,
        // CryptoNote-only concepts.
        double_spends: None,
        oldest: None,
    })
}

/// Combine `getblockchaininfo` and `getnetworkinfo` response bodies.
pub fn parse_node_info(
    blockchain_body: &[u8],
    network_body: &[u8],
) -> Result<NodeInfo, AdapterError> {
    let chain: BlockchainInfo = jsonrpc_result(blockchain_body)?;
    let network: NetworkInfo = jsonrpc_result(network_body)?;

    // subversion is wrapped in slashes ("/Satoshi:28.0.0/"); strip them so the
    // UI can print it as-is.
    let version = match (network.subversion, network.version) {
        (Some(subversion), _) => subversion.trim_matches('/').to_string(),
        (None, Some(version)) => version.to_string(),
        (None, None) => String::new(),
    };

    Ok(NodeInfo {
        height: chain.blocks,
        version,
        peer_count: network.connections,
    })
}

fn fee_per_vbyte(fee_atomic: u128, vsize: u64) -> f64 {
    if vsize == 0 {
        return 0.0;
    }
    fee_atomic as f64 / vsize as f64
}

fn unix_time(seconds: i64) -> Option<SystemTime> {
    if seconds <= 0 {
        return None;
    }
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds as u64))
}
