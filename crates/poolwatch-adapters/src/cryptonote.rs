//! CryptoNote-family adapter: monerod, oxend, beldexd and compatible forks.
//!
//! Field names below were read off the daemons' own
//! `src/rpc/core_rpc_server_commands_defs.h`; see [`QuirkTable`] for the
//! instant-transaction (Blink/Flash) specifics and what is verified vs. not.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use poolwatch_core::{
    AdapterError, ChainAdapter, ChainMeta, InstantTxKind, MempoolSnapshot, PoolStats, PoolTx,
};
use serde::Deserialize;
use serde_json::Value;

use crate::rpc::{
    join_url, parse_json, Auth, DigestAuth, HttpClient, NodeInfo, ParsedPool, SMALL_RESPONSE_LIMIT,
};

/// A daemon `status` string other than "OK" carries no numeric code, so we
/// report a synthetic one.
const DAEMON_STATUS_CODE: i64 = -1;

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
    http: HttpClient,
    quirks: QuirkTable,
}

impl CryptonoteAdapter {
    pub fn new(config: CryptonoteConfig) -> Result<Self, AdapterError> {
        let auth = match &config.rpc_login {
            Some(login) => Auth::Digest(DigestAuth::from_login(login)?),
            None => Auth::None,
        };
        let quirks = QuirkTable::for_kind(config.instant_tx);
        Ok(Self {
            config,
            http: HttpClient::new(auth)?,
            quirks,
        })
    }

    fn url(&self, path: &str) -> String {
        join_url(&self.config.rpc_url, path)
    }
}

#[async_trait]
impl ChainAdapter for CryptonoteAdapter {
    async fn fetch_snapshot(&self) -> Result<MempoolSnapshot, AdapterError> {
        let taken_at = SystemTime::now();

        let pool_body = self
            .http
            .post_json(
                &self.url("get_transaction_pool"),
                b"{}",
                self.config.max_pool_response_bytes,
            )
            .await?;
        let pool = parse_transaction_pool(&pool_body, taken_at, &self.quirks)?;

        let info_body = self
            .http
            .post_json(
                &self.url("json_rpc"),
                br#"{"jsonrpc":"2.0","id":"0","method":"get_info"}"#,
                SMALL_RESPONSE_LIMIT,
            )
            .await?;
        let info = parse_get_info(&info_body)?;

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
            .http
            .post_json(
                &self.url("get_transaction_pool_stats"),
                b"{}",
                SMALL_RESPONSE_LIMIT,
            )
            .await?;
        parse_pool_stats(&body, &self.config.meta.id, taken_at)
    }

    fn chain_meta(&self) -> &ChainMeta {
        &self.config.meta
    }
}

// ---------------------------------------------------------------------------
// Instant-transaction (Oxen Blink / Beldex Flash) detection.
// ---------------------------------------------------------------------------

/// Per-fork knowledge about how an instant transaction is marked in the pool
/// RPC response. Detection tries the top-level per-tx boolean first, then the
/// (unverified) tx_json probes; anything unrecognized degrades to
/// [`InstantTxKind::None`] with a single warning per poll.
///
/// VERIFIED, Oxen: `tx_info` carries `bool blink; // True if this is a signed
/// blink transaction`
/// (oxen-io/oxen-core, src/rpc/core_rpc_server_commands_defs.h, tag v8.1.3:
/// <https://raw.githubusercontent.com/oxen-io/oxen-core/v8.1.3/src/rpc/core_rpc_server_commands_defs.h>).
///
/// VERIFIED, Beldex: the same struct carries `bool flash; // True if this is a
/// signed flash transaction` — Beldex renamed Oxen's `blink`
/// (Beldex-Coin/beldex, src/rpc/core_rpc_server_commands_defs.h, tag v4.0.0:
/// <https://raw.githubusercontent.com/Beldex-Coin/beldex/v4.0.0/src/rpc/core_rpc_server_commands_defs.h>).
/// Older Beldex builds forked from Oxen before the rename may still emit
/// `blink`, so Flash probes both names.
///
/// VERIFIED (negative), tx_json: neither daemon has a Blink/Flash transaction
/// *type*. `txtype` is {standard, state_change, key_image_unlock, stake,
/// oxen_name_system, ...} for Oxen and {standard, state_change,
/// key_image_unlock, stake, beldex_name_system, coin_burn} for Beldex
/// (src/cryptonote_basic/txtypes.h in both). Instant-ness is a mempool/quorum
/// property, not part of the serialized transaction. The tx_json probes below
/// are therefore UNVERIFIED defensive fallbacks for forks that might surface
/// the flag there; no integer type value is defensible, so only the textual
/// names are matched.
///
/// Monero upstream has no instant transactions at all — `instant_tx = None`
/// disables everything here.
#[derive(Debug, Clone)]
pub struct QuirkTable {
    kind: InstantTxKind,
    /// Top-level per-tx boolean field names (VERIFIED, see above).
    top_level_flags: &'static [&'static str],
    /// tx_json boolean field names (UNVERIFIED fallback).
    tx_json_flags: &'static [&'static str],
    /// tx_json fields naming a transaction type (UNVERIFIED fallback).
    tx_json_type_fields: &'static [&'static str],
    /// tx_json type values that would mark an instant tx (UNVERIFIED).
    tx_json_type_values: &'static [&'static str],
}

impl QuirkTable {
    /// Build the table for a configured chain. `None` (or an explicit
    /// `Some(None)` kind) disables detection entirely.
    pub fn for_kind(kind: Option<InstantTxKind>) -> Self {
        match kind {
            Some(InstantTxKind::Blink) => Self {
                kind: InstantTxKind::Blink,
                top_level_flags: &["blink"],
                tx_json_flags: &["blink"],
                tx_json_type_fields: &["type", "tx_type"],
                tx_json_type_values: &["blink"],
            },
            Some(InstantTxKind::Flash) => Self {
                kind: InstantTxKind::Flash,
                // Beldex renamed blink -> flash; accept both for old builds.
                top_level_flags: &["flash", "blink"],
                tx_json_flags: &["flash", "blink"],
                tx_json_type_fields: &["type", "tx_type"],
                tx_json_type_values: &["flash", "blink"],
            },
            Some(InstantTxKind::None) | None => Self {
                kind: InstantTxKind::None,
                top_level_flags: &[],
                tx_json_flags: &[],
                tx_json_type_fields: &[],
                tx_json_type_values: &[],
            },
        }
    }

    fn enabled(&self) -> bool {
        self.kind != InstantTxKind::None
    }

    fn detect(&self, extra: &serde_json::Map<String, Value>, tx_json: Option<&Value>) -> Detection {
        if !self.enabled() {
            return Detection::default();
        }

        for name in self.top_level_flags {
            match extra.get(*name) {
                None => continue,
                Some(Value::Bool(flag)) => {
                    return Detection {
                        kind: if *flag {
                            self.kind
                        } else {
                            InstantTxKind::None
                        },
                        flag_field_present: true,
                        flag_field_unusable: false,
                    };
                }
                Some(_) => {
                    return Detection {
                        kind: InstantTxKind::None,
                        flag_field_present: true,
                        flag_field_unusable: true,
                    };
                }
            }
        }

        // UNVERIFIED fallbacks, only reached when the daemon does not report
        // the top-level boolean at all.
        if let Some(Value::Object(tx)) = tx_json {
            for name in self.tx_json_flags {
                if let Some(Value::Bool(flag)) = tx.get(*name) {
                    return Detection {
                        kind: if *flag {
                            self.kind
                        } else {
                            InstantTxKind::None
                        },
                        flag_field_present: true,
                        flag_field_unusable: false,
                    };
                }
            }
            for name in self.tx_json_type_fields {
                let Some(value) = tx.get(*name) else { continue };
                let Some(text) = value.as_str() else { continue };
                if self
                    .tx_json_type_values
                    .iter()
                    .any(|candidate| text.eq_ignore_ascii_case(candidate))
                {
                    return Detection {
                        kind: self.kind,
                        flag_field_present: true,
                        flag_field_unusable: false,
                    };
                }
            }
        }

        Detection::default()
    }
}

#[derive(Default)]
struct Detection {
    kind: InstantTxKind,
    flag_field_present: bool,
    flag_field_unusable: bool,
}

/// Counters aggregated over one pool parse so warnings are emitted once per
/// poll instead of once per transaction.
#[derive(Default)]
struct ParseReport {
    txs: usize,
    instant_flag_seen: usize,
    instant_flag_unusable: usize,
    tx_json_failures: usize,
    first_bad_tx_json: Option<String>,
}

impl ParseReport {
    fn emit(&self, quirks: &QuirkTable) {
        if self.tx_json_failures > 0 {
            tracing::warn!(
                failures = self.tx_json_failures,
                txid = self.first_bad_tx_json.as_deref().unwrap_or(""),
                "could not parse tx_json for some pool transactions; \
                 input/output counts unavailable for them"
            );
        }
        if !quirks.enabled() {
            return;
        }
        if self.instant_flag_unusable > 0 {
            tracing::warn!(
                count = self.instant_flag_unusable,
                "instant-tx flag field had an unexpected type; treating those \
                 transactions as non-instant"
            );
        } else if self.txs > 0 && self.instant_flag_seen == 0 {
            tracing::warn!(
                txs = self.txs,
                fields = ?quirks.top_level_flags,
                "daemon reported no instant-tx flag on any pool transaction; \
                 instant-tx detection appears unsupported by this daemon"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Wire types.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PoolResponse {
    #[serde(default)]
    status: Option<String>,
    /// Absent entirely when the pool is empty on some daemon versions.
    #[serde(default)]
    transactions: Vec<PoolTxRaw>,
}

#[derive(Deserialize)]
struct PoolTxRaw {
    /// monerod/oxend/beldexd all name the txid `id_hash` in `tx_info`.
    /// `tx_hash`/`txid` are accepted for proxies and forks that rename it.
    #[serde(default)]
    id_hash: Option<String>,
    #[serde(default)]
    tx_hash: Option<String>,
    #[serde(default)]
    txid: Option<String>,
    #[serde(default)]
    blob_size: u64,
    /// Unix seconds; 0 (or absent on some forks) means "not reported".
    #[serde(default)]
    receive_time: i64,
    #[serde(default)]
    fee: u64,
    #[serde(default)]
    double_spend_seen: bool,
    /// Stringified JSON of the transaction itself.
    #[serde(default)]
    tx_json: Option<String>,
    /// Everything else, so instant-tx flags can be probed by name.
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

#[derive(Deserialize)]
struct PoolStatsResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    pool_stats: Option<PoolStatsRaw>,
}

#[derive(Deserialize)]
struct PoolStatsRaw {
    #[serde(default)]
    txs_total: u64,
    #[serde(default)]
    bytes_total: u64,
    #[serde(default)]
    fee_total: Option<u64>,
    #[serde(default)]
    num_double_spends: Option<u64>,
    #[serde(default)]
    oldest: Option<i64>,
}

#[derive(Deserialize)]
struct GetInfoResult {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    height: u64,
    /// A string on modern monerod ("0.18.4.0-release"); older forks and some
    /// clones report an integer.
    #[serde(default)]
    version: Option<Value>,
    #[serde(default)]
    outgoing_connections_count: Option<u32>,
    #[serde(default)]
    incoming_connections_count: Option<u32>,
}

// ---------------------------------------------------------------------------
// Pure parsers (fixture-tested).
// ---------------------------------------------------------------------------

/// Parse a `/get_transaction_pool` response body.
pub fn parse_transaction_pool(
    body: &[u8],
    now: SystemTime,
    quirks: &QuirkTable,
) -> Result<ParsedPool, AdapterError> {
    let response: PoolResponse = parse_json(body)?;
    check_status(response.status.as_deref())?;

    let mut report = ParseReport {
        txs: response.transactions.len(),
        ..ParseReport::default()
    };
    let mut txs = Vec::with_capacity(response.transactions.len());
    let mut pool_bytes: u64 = 0;

    for raw in response.transactions {
        let txid =
            raw.id_hash.or(raw.tx_hash).or(raw.txid).ok_or_else(|| {
                AdapterError::Parse("pool transaction has no id_hash".to_string())
            })?;

        // A malformed tx_json must never sink the whole snapshot.
        let tx_json = match raw.tx_json.as_deref() {
            None => None,
            Some(text) => match serde_json::from_str::<Value>(text) {
                Ok(value) => Some(value),
                Err(_) => {
                    report.tx_json_failures += 1;
                    report.first_bad_tx_json.get_or_insert_with(|| txid.clone());
                    None
                }
            },
        };
        let in_out_counts = tx_json.as_ref().and_then(in_out_counts);

        let detection = quirks.detect(&raw.extra, tx_json.as_ref());
        report.instant_flag_seen += usize::from(detection.flag_field_present);
        report.instant_flag_unusable += usize::from(detection.flag_field_unusable);

        pool_bytes = pool_bytes.saturating_add(raw.blob_size);
        txs.push(PoolTx {
            txid,
            size_bytes: raw.blob_size,
            fee_atomic: u128::from(raw.fee),
            fee_per_byte: fee_per_byte(raw.fee, raw.blob_size),
            received_by_node: unix_time(raw.receive_time),
            first_seen_local: now,
            instant: detection.kind,
            in_out_counts,
            double_spend_seen: raw.double_spend_seen,
        });
    }

    report.emit(quirks);
    Ok(ParsedPool { txs, pool_bytes })
}

/// Parse a `/get_transaction_pool_stats` response body.
pub fn parse_pool_stats(
    body: &[u8],
    chain_id: &str,
    taken_at: SystemTime,
) -> Result<PoolStats, AdapterError> {
    let response: PoolStatsResponse = parse_json(body)?;
    check_status(response.status.as_deref())?;
    let stats = response
        .pool_stats
        .ok_or_else(|| AdapterError::Parse("response has no pool_stats object".to_string()))?;

    Ok(PoolStats {
        chain_id: chain_id.to_string(),
        taken_at,
        tx_count: stats.txs_total,
        pool_bytes: stats.bytes_total,
        fee_total: stats.fee_total.map(u128::from),
        double_spends: stats.num_double_spends,
        oldest: stats.oldest.and_then(unix_time),
    })
}

/// Parse a `/json_rpc` `get_info` response body.
pub fn parse_get_info(body: &[u8]) -> Result<NodeInfo, AdapterError> {
    let result: GetInfoResult = crate::rpc::jsonrpc_result(body)?;
    check_status(result.status.as_deref())?;

    let version = match result.version {
        Some(Value::String(text)) => text,
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    };
    let peer_count = match (
        result.outgoing_connections_count,
        result.incoming_connections_count,
    ) {
        (None, None) => None,
        (out, incoming) => Some(out.unwrap_or(0).saturating_add(incoming.unwrap_or(0))),
    };

    Ok(NodeInfo {
        height: result.height,
        version,
        peer_count,
    })
}

/// `status` is "OK" on success and e.g. "BUSY" while the node is syncing.
/// Some forks omit it, which is not an error.
fn check_status(status: Option<&str>) -> Result<(), AdapterError> {
    match status {
        None => Ok(()),
        Some(text) if text.eq_ignore_ascii_case("OK") => Ok(()),
        Some(text) => Err(AdapterError::Rpc {
            code: DAEMON_STATUS_CODE,
            message: format!("daemon status: {text}"),
        }),
    }
}

/// (ring inputs, outputs) from a parsed tx_json.
fn in_out_counts(tx_json: &Value) -> Option<(u16, u16)> {
    let vin = tx_json.get("vin")?.as_array()?.len();
    let vout = tx_json.get("vout")?.as_array()?.len();
    Some((
        vin.min(u16::MAX as usize) as u16,
        vout.min(u16::MAX as usize) as u16,
    ))
}

fn fee_per_byte(fee: u64, size_bytes: u64) -> f64 {
    if size_bytes == 0 {
        return 0.0;
    }
    fee as f64 / size_bytes as f64
}

/// Daemon-reported unix seconds; 0 (and anything negative) means "unknown".
fn unix_time(seconds: i64) -> Option<SystemTime> {
    if seconds <= 0 {
        return None;
    }
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds as u64))
}
