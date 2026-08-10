//! Configuration file parsing, validation, and adapter construction.
//!
//! `poolwatch.toml`'s exact shape is documented in `poolwatch.example.toml`
//! at the repo root; keep this file's schema in sync with it by hand (there
//! is no shared source of truth to derive from — the example file is not
//! ours to touch).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use poolwatch_adapters::{
    BitcoindAdapter, BitcoindAuth, BitcoindConfig, CryptonoteAdapter, CryptonoteConfig,
};
use poolwatch_core::{AdapterError, ChainAdapter, ChainMeta, InstantTxKind};
use serde::Deserialize;
use tracing::warn;

const DEFAULT_MAX_POOL_RESPONSE_MB: u64 = 64;
const DEFAULT_CRYPTONOTE_FEE_UNIT: &str = "au/B";
const DEFAULT_BITCOIND_FEE_UNIT: &str = "sat/vB";

const GENERAL_KEYS: &[&str] = &[
    "poll_interval_secs",
    "stats_interval_secs",
    "log_file",
    "max_pool_response_mb",
];

const CHAIN_KEYS: &[&str] = &[
    "id",
    "display_name",
    "ticker",
    "family",
    "rpc_url",
    "decimals",
    "fee_rate_unit",
    "instant_tx",
    "rpc_login",
    "rpc_cookie_file",
    "rpc_user",
    "rpc_password",
];

const TOP_LEVEL_KEYS: &[&str] = &["general", "chain"];

/// Raw `[general]` table, as written in the TOML file.
#[derive(Debug, Clone, Deserialize)]
struct GeneralFile {
    poll_interval_secs: u64,
    stats_interval_secs: u64,
    log_file: String,
    #[serde(default)]
    max_pool_response_mb: Option<u64>,
}

/// Raw `[[chain]]` table, as written in the TOML file.
#[derive(Debug, Clone, Deserialize)]
struct ChainFile {
    id: String,
    display_name: String,
    ticker: String,
    family: String,
    rpc_url: String,
    decimals: u32,
    #[serde(default)]
    fee_rate_unit: Option<String>,
    #[serde(default)]
    instant_tx: Option<String>,
    #[serde(default)]
    rpc_login: Option<String>,
    #[serde(default)]
    rpc_cookie_file: Option<String>,
    #[serde(default)]
    rpc_user: Option<String>,
    #[serde(default)]
    rpc_password: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FileConfig {
    general: GeneralFile,
    #[serde(rename = "chain", default)]
    chains: Vec<ChainFile>,
}

/// Family-specific, validated chain configuration.
#[derive(Debug, Clone)]
pub enum ChainFamilyCfg {
    Cryptonote {
        rpc_login: Option<String>,
        instant_tx: Option<InstantTxKind>,
    },
    Bitcoind {
        auth: BitcoindAuth,
    },
}

/// Fully validated, defaulted, `~`-expanded chain configuration, ready to
/// build an adapter from and to drive the UI.
#[derive(Debug, Clone)]
pub struct ChainRuntimeCfg {
    pub meta: ChainMeta,
    pub rpc_url: String,
    pub max_pool_response_bytes: u64,
    pub family: ChainFamilyCfg,
}

impl ChainRuntimeCfg {
    pub fn is_cryptonote(&self) -> bool {
        matches!(self.family, ChainFamilyCfg::Cryptonote { .. })
    }
}

/// Fully validated, defaulted runtime configuration.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub poll_interval: Duration,
    pub stats_interval: Duration,
    pub log_file: PathBuf,
    pub chains: Vec<ChainRuntimeCfg>,
}

impl RuntimeConfig {
    /// Applies `--interval`, overriding `poll_interval_secs` from the file.
    pub fn override_poll_interval(&mut self, secs: Option<u64>) {
        if let Some(secs) = secs {
            self.poll_interval = Duration::from_secs(secs);
        }
    }

    /// Applies repeated `--chain` filters. Errors, listing valid ids, if any
    /// requested id doesn't match a configured chain. A no-op if `ids` is
    /// empty (the default: watch every configured chain).
    pub fn restrict_to_chains(&mut self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let valid: Vec<&str> = self.chains.iter().map(|c| c.meta.id.as_str()).collect();
        for id in ids {
            if !valid.contains(&id.as_str()) {
                bail!("unknown chain id '{id}'; valid ids: {}", valid.join(", "));
            }
        }
        self.chains
            .retain(|c| ids.iter().any(|id| id == &c.meta.id));
        Ok(())
    }
}

/// Reads the config file's raw text. Kept separate from [`parse`] so
/// `main` can peek at `[general].log_file` and set up logging before the
/// full (warning-emitting) parse runs.
pub fn read_config_text(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| {
        format!(
            "failed to read config file '{}' — copy poolwatch.example.toml to get started, \
             or pass --config <path> to point at a different file",
            path.display()
        )
    })
}

/// Extracts and `~`-expands `[general].log_file` without validating the
/// rest of the file. Used to stand up logging before the full parse (whose
/// unknown-key warnings should land in the log, not stderr).
pub fn peek_log_file(text: &str) -> Result<PathBuf> {
    let value: toml::Value = toml::from_str(text).context("failed to parse config file as TOML")?;
    let raw = value
        .get("general")
        .and_then(|g| g.get("log_file"))
        .and_then(|v| v.as_str())
        .context("config is missing [general].log_file")?;
    Ok(PathBuf::from(shellexpand::tilde(raw).into_owned()))
}

/// Parses and validates already-read config text. Unknown keys are logged
/// via `tracing::warn!` but never fail the load.
pub fn parse(text: &str) -> Result<RuntimeConfig> {
    warn_unknown_keys(text);

    let file: FileConfig =
        toml::from_str(text).context("failed to parse config file (schema mismatch)")?;

    validate_and_build(file)
}

/// Parses the raw TOML into a generic [`toml::Value`] and warns (does not
/// fail) on any key not in the known schema, at both the `[general]` level
/// and per `[[chain]]` block.
fn warn_unknown_keys(text: &str) {
    let Ok(value) = toml::from_str::<toml::Value>(text) else {
        return; // the typed parse that follows will surface a real error
    };
    let Some(table) = value.as_table() else {
        return;
    };

    warn_unknown(table, TOP_LEVEL_KEYS, "top level".to_string());

    if let Some(general) = table.get("general").and_then(|v| v.as_table()) {
        warn_unknown(general, GENERAL_KEYS, "[general]".to_string());
    }
    if let Some(chains) = table.get("chain").and_then(|v| v.as_array()) {
        for (i, chain) in chains.iter().enumerate() {
            if let Some(t) = chain.as_table() {
                let id = t
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("#{i}"));
                warn_unknown(t, CHAIN_KEYS, format!("[[chain]] '{id}'"));
            }
        }
    }
}

fn warn_unknown(table: &toml::Table, known: &[&str], context: String) {
    for key in table.keys() {
        if !known.contains(&key.as_str()) {
            warn!("config: {context}: unknown key '{key}' (ignored)");
        }
    }
}

fn validate_and_build(file: FileConfig) -> Result<RuntimeConfig> {
    if file.chains.is_empty() {
        bail!("config must define at least one [[chain]] block");
    }

    let mut seen_ids = HashSet::new();
    let mut chains = Vec::with_capacity(file.chains.len());
    for chain in file.chains {
        if !seen_ids.insert(chain.id.clone()) {
            bail!("duplicate chain id '{}'", chain.id);
        }
        chains.push(build_chain(chain, file.general.max_pool_response_mb)?);
    }

    let log_file = PathBuf::from(shellexpand::tilde(&file.general.log_file).into_owned());

    Ok(RuntimeConfig {
        poll_interval: Duration::from_secs(file.general.poll_interval_secs),
        stats_interval: Duration::from_secs(file.general.stats_interval_secs),
        log_file,
        chains,
    })
}

fn build_chain(chain: ChainFile, general_max_mb: Option<u64>) -> Result<ChainRuntimeCfg> {
    let max_pool_response_bytes =
        general_max_mb.unwrap_or(DEFAULT_MAX_POOL_RESPONSE_MB) * 1024 * 1024;

    let family = match chain.family.as_str() {
        "cryptonote" => build_cryptonote_family(&chain)?,
        "bitcoind" => build_bitcoind_family(&chain)?,
        other => bail!(
            "chain '{}': family must be 'cryptonote' or 'bitcoind', got '{other}'",
            chain.id
        ),
    };

    let fee_rate_unit = chain.fee_rate_unit.clone().unwrap_or_else(|| match family {
        ChainFamilyCfg::Bitcoind { .. } => DEFAULT_BITCOIND_FEE_UNIT.to_string(),
        ChainFamilyCfg::Cryptonote { .. } => DEFAULT_CRYPTONOTE_FEE_UNIT.to_string(),
    });

    Ok(ChainRuntimeCfg {
        meta: ChainMeta {
            id: chain.id,
            display_name: chain.display_name,
            ticker: chain.ticker,
            decimals: chain.decimals,
            fee_rate_unit,
        },
        rpc_url: chain.rpc_url,
        max_pool_response_bytes,
        family,
    })
}

fn build_cryptonote_family(chain: &ChainFile) -> Result<ChainFamilyCfg> {
    if chain.rpc_cookie_file.is_some() || chain.rpc_user.is_some() || chain.rpc_password.is_some() {
        bail!(
            "chain '{}': cryptonote chains must not set rpc_cookie_file/rpc_user/rpc_password \
             (those are bitcoind-only)",
            chain.id
        );
    }
    let instant_tx = match chain.instant_tx.as_deref() {
        None => None,
        Some("blink") => Some(InstantTxKind::Blink),
        Some("flash") => Some(InstantTxKind::Flash),
        Some(other) => bail!(
            "chain '{}': instant_tx must be 'blink' or 'flash', got '{other}'",
            chain.id
        ),
    };
    Ok(ChainFamilyCfg::Cryptonote {
        rpc_login: chain.rpc_login.clone(),
        instant_tx,
    })
}

fn build_bitcoind_family(chain: &ChainFile) -> Result<ChainFamilyCfg> {
    if chain.rpc_login.is_some() {
        bail!(
            "chain '{}': bitcoind chains must not set rpc_login (cryptonote-only)",
            chain.id
        );
    }
    if chain.instant_tx.is_some() {
        bail!(
            "chain '{}': instant_tx is only valid for cryptonote chains",
            chain.id
        );
    }

    let has_cookie = chain.rpc_cookie_file.is_some();
    let has_user = chain.rpc_user.is_some();
    let has_password = chain.rpc_password.is_some();
    let has_userpass = has_user || has_password;

    if has_cookie && has_userpass {
        bail!(
            "chain '{}': set exactly one of rpc_cookie_file or rpc_user+rpc_password, not both",
            chain.id
        );
    }
    if !has_cookie && !has_userpass {
        bail!(
            "chain '{}': bitcoind requires rpc_cookie_file or rpc_user+rpc_password",
            chain.id
        );
    }
    if has_userpass && has_user != has_password {
        bail!(
            "chain '{}': rpc_user and rpc_password must be set together",
            chain.id
        );
    }

    let auth = if has_cookie {
        // Safe: has_cookie implies this is Some.
        let raw = chain.rpc_cookie_file.as_deref().unwrap();
        BitcoindAuth::CookieFile(PathBuf::from(shellexpand::tilde(raw).into_owned()))
    } else {
        // Safe: the checks above leave only the has_user==has_password==true
        // case here.
        BitcoindAuth::UserPass {
            user: chain.rpc_user.clone().unwrap(),
            password: chain.rpc_password.clone().unwrap(),
        }
    };
    Ok(ChainFamilyCfg::Bitcoind { auth })
}

/// Builds a concrete adapter for one chain config.
pub fn build_adapter(cfg: &ChainRuntimeCfg) -> Result<Arc<dyn ChainAdapter>, AdapterError> {
    match &cfg.family {
        ChainFamilyCfg::Cryptonote {
            rpc_login,
            instant_tx,
        } => {
            let adapter = CryptonoteAdapter::new(CryptonoteConfig {
                meta: cfg.meta.clone(),
                rpc_url: cfg.rpc_url.clone(),
                rpc_login: rpc_login.clone(),
                instant_tx: *instant_tx,
                max_pool_response_bytes: cfg.max_pool_response_bytes,
            })?;
            Ok(Arc::new(adapter))
        }
        ChainFamilyCfg::Bitcoind { auth } => {
            let adapter = BitcoindAdapter::new(BitcoindConfig {
                meta: cfg.meta.clone(),
                rpc_url: cfg.rpc_url.clone(),
                auth: auth.clone(),
            })?;
            Ok(Arc::new(adapter))
        }
    }
}

/// One configured chain's runtime config paired with its constructed
/// adapter.
pub type AdapterPair = (ChainRuntimeCfg, Arc<dyn ChainAdapter>);

/// Builds adapters for every configured chain, pairing each with its
/// runtime config for the poller/UI layers.
pub fn build_adapters(cfg: &RuntimeConfig) -> Result<Vec<AdapterPair>, AdapterError> {
    cfg.chains
        .iter()
        .cloned()
        .map(|c| {
            let adapter = build_adapter(&c)?;
            Ok((c, adapter))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
        [general]
        poll_interval_secs = 5
        stats_interval_secs = 2
        log_file = "~/.local/state/poolwatch/poolwatch.log"

        [[chain]]
        id = "monero-main"
        display_name = "Monero"
        ticker = "XMR"
        family = "cryptonote"
        rpc_url = "http://127.0.0.1:18081"
        decimals = 12
        fee_rate_unit = "pN/B"

        [[chain]]
        id = "oxen-main"
        display_name = "Oxen"
        ticker = "OXEN"
        family = "cryptonote"
        rpc_url = "http://127.0.0.1:22023"
        decimals = 9
        instant_tx = "blink"

        [[chain]]
        id = "bitcoin-main"
        display_name = "Bitcoin"
        ticker = "BTC"
        family = "bitcoind"
        rpc_url = "http://127.0.0.1:8332"
        decimals = 8
        rpc_cookie_file = "~/.bitcoin/.cookie"
    "#;

    #[test]
    fn parses_valid_example_shaped_config() {
        let cfg = parse(VALID).expect("valid config should parse");
        assert_eq!(cfg.poll_interval, Duration::from_secs(5));
        assert_eq!(cfg.stats_interval, Duration::from_secs(2));
        assert_eq!(cfg.chains.len(), 3);

        let monero = &cfg.chains[0];
        assert_eq!(monero.meta.id, "monero-main");
        assert_eq!(monero.meta.fee_rate_unit, "pN/B"); // explicit override kept
        assert!(monero.is_cryptonote());

        let oxen = &cfg.chains[1];
        assert_eq!(oxen.meta.fee_rate_unit, DEFAULT_CRYPTONOTE_FEE_UNIT); // defaulted
        match &oxen.family {
            ChainFamilyCfg::Cryptonote { instant_tx, .. } => {
                assert_eq!(*instant_tx, Some(InstantTxKind::Blink));
            }
            ChainFamilyCfg::Bitcoind { .. } => panic!("oxen should be cryptonote"),
        }

        let btc = &cfg.chains[2];
        assert_eq!(btc.meta.fee_rate_unit, DEFAULT_BITCOIND_FEE_UNIT); // defaulted
        assert!(!btc.is_cryptonote());
        assert_eq!(
            btc.max_pool_response_bytes,
            DEFAULT_MAX_POOL_RESPONSE_MB * 1024 * 1024
        );
        match &btc.family {
            ChainFamilyCfg::Bitcoind {
                auth: BitcoindAuth::CookieFile(path),
            } => {
                // ~ must have been expanded away.
                assert!(!path.to_string_lossy().starts_with('~'));
                assert!(path.to_string_lossy().ends_with(".bitcoin/.cookie"));
            }
            other => panic!("expected cookie-file auth, got {other:?}"),
        }

        // log_file's ~ must also be expanded.
        assert!(!cfg.log_file.to_string_lossy().starts_with('~'));
    }

    #[test]
    fn honors_explicit_max_pool_response_mb() {
        let text = VALID.replacen(
            "[general]",
            "[general]\n        max_pool_response_mb = 128",
            1,
        );
        let cfg = parse(&text).unwrap();
        assert_eq!(cfg.chains[0].max_pool_response_bytes, 128 * 1024 * 1024);
    }

    #[test]
    fn requires_at_least_one_chain() {
        let text = r#"
            [general]
            poll_interval_secs = 5
            stats_interval_secs = 2
            log_file = "poolwatch.log"
        "#;
        let err = parse(text).unwrap_err();
        assert!(err.to_string().contains("at least one"));
    }

    #[test]
    fn rejects_duplicate_chain_ids() {
        let text = format!(
            "{VALID}\n{}",
            r#"
            [[chain]]
            id = "monero-main"
            display_name = "Monero 2"
            ticker = "XMR"
            family = "cryptonote"
            rpc_url = "http://127.0.0.1:18082"
            decimals = 12
            "#
        );
        let err = parse(&text).unwrap_err();
        assert!(err.to_string().contains("duplicate chain id"));
    }

    #[test]
    fn rejects_unknown_family() {
        let text = r#"
            [general]
            poll_interval_secs = 5
            stats_interval_secs = 2
            log_file = "poolwatch.log"

            [[chain]]
            id = "x"
            display_name = "X"
            ticker = "X"
            family = "zcashd"
            rpc_url = "http://127.0.0.1:1"
            decimals = 8
        "#;
        let err = parse(text).unwrap_err();
        assert!(err.to_string().contains("family must be"));
    }

    fn bitcoind_chain(extra: &str) -> String {
        format!(
            r#"
            [general]
            poll_interval_secs = 5
            stats_interval_secs = 2
            log_file = "poolwatch.log"

            [[chain]]
            id = "btc"
            display_name = "Bitcoin"
            ticker = "BTC"
            family = "bitcoind"
            rpc_url = "http://127.0.0.1:8332"
            decimals = 8
            {extra}
            "#
        )
    }

    #[test]
    fn bitcoind_requires_auth() {
        let text = bitcoind_chain("");
        let err = parse(&text).unwrap_err();
        assert!(err.to_string().contains("requires rpc_cookie_file"));
    }

    #[test]
    fn bitcoind_rejects_both_auth_methods() {
        let text = bitcoind_chain(
            r#"rpc_cookie_file = "~/.bitcoin/.cookie"
            rpc_user = "u"
            rpc_password = "p""#,
        );
        let err = parse(&text).unwrap_err();
        assert!(err.to_string().contains("not both"));
    }

    #[test]
    fn bitcoind_rejects_user_without_password() {
        let text = bitcoind_chain(r#"rpc_user = "u""#);
        let err = parse(&text).unwrap_err();
        assert!(err.to_string().contains("must be set together"));
    }

    #[test]
    fn bitcoind_rejects_password_without_user() {
        let text = bitcoind_chain(r#"rpc_password = "p""#);
        let err = parse(&text).unwrap_err();
        assert!(err.to_string().contains("must be set together"));
    }

    #[test]
    fn bitcoind_accepts_user_and_password() {
        let text = bitcoind_chain(
            r#"rpc_user = "u"
            rpc_password = "p""#,
        );
        let cfg = parse(&text).unwrap();
        match &cfg.chains[0].family {
            ChainFamilyCfg::Bitcoind {
                auth: BitcoindAuth::UserPass { user, password },
            } => {
                assert_eq!(user, "u");
                assert_eq!(password, "p");
            }
            other => panic!("expected user/pass auth, got {other:?}"),
        }
    }

    #[test]
    fn bitcoind_rejects_rpc_login() {
        let text = bitcoind_chain(
            r#"rpc_cookie_file = "~/.bitcoin/.cookie"
            rpc_login = "user:pass""#,
        );
        let err = parse(&text).unwrap_err();
        assert!(err.to_string().contains("rpc_login"));
    }

    #[test]
    fn bitcoind_rejects_instant_tx() {
        let text = bitcoind_chain(
            r#"rpc_cookie_file = "~/.bitcoin/.cookie"
            instant_tx = "blink""#,
        );
        let err = parse(&text).unwrap_err();
        assert!(err.to_string().contains("instant_tx"));
    }

    #[test]
    fn cryptonote_rejects_bitcoind_auth_keys() {
        let text = r#"
            [general]
            poll_interval_secs = 5
            stats_interval_secs = 2
            log_file = "poolwatch.log"

            [[chain]]
            id = "xmr"
            display_name = "Monero"
            ticker = "XMR"
            family = "cryptonote"
            rpc_url = "http://127.0.0.1:18081"
            decimals = 12
            rpc_user = "u"
        "#;
        let err = parse(text).unwrap_err();
        assert!(err
            .to_string()
            .contains("rpc_cookie_file/rpc_user/rpc_password"));
    }

    #[test]
    fn cryptonote_rejects_bad_instant_tx_value() {
        let text = r#"
            [general]
            poll_interval_secs = 5
            stats_interval_secs = 2
            log_file = "poolwatch.log"

            [[chain]]
            id = "xmr"
            display_name = "Monero"
            ticker = "XMR"
            family = "cryptonote"
            rpc_url = "http://127.0.0.1:18081"
            decimals = 12
            instant_tx = "turbo"
        "#;
        let err = parse(text).unwrap_err();
        assert!(err.to_string().contains("blink"));
    }

    #[test]
    fn unknown_keys_warn_but_do_not_fail() {
        let text = VALID.replacen("[general]", "[general]\n        totally_unknown_key = 1", 1);
        // Must still parse successfully; unknown keys only warn.
        assert!(parse(&text).is_ok());

        let text2 = format!("{VALID}\nnot_a_real_top_level_table = 1\n");
        assert!(parse(&text2).is_ok());
    }

    #[test]
    fn restrict_to_chains_filters_and_validates() {
        let mut cfg = parse(VALID).unwrap();
        cfg.restrict_to_chains(&["oxen-main".to_string()]).unwrap();
        assert_eq!(cfg.chains.len(), 1);
        assert_eq!(cfg.chains[0].meta.id, "oxen-main");
    }

    #[test]
    fn restrict_to_chains_errors_on_unknown_id() {
        let mut cfg = parse(VALID).unwrap();
        let err = cfg
            .restrict_to_chains(&["not-a-chain".to_string()])
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not-a-chain"));
        assert!(msg.contains("monero-main"));
    }

    #[test]
    fn restrict_to_chains_empty_is_noop() {
        let mut cfg = parse(VALID).unwrap();
        cfg.restrict_to_chains(&[]).unwrap();
        assert_eq!(cfg.chains.len(), 3);
    }

    #[test]
    fn override_poll_interval_applies_when_some() {
        let mut cfg = parse(VALID).unwrap();
        cfg.override_poll_interval(Some(30));
        assert_eq!(cfg.poll_interval, Duration::from_secs(30));
    }

    #[test]
    fn override_poll_interval_noop_when_none() {
        let mut cfg = parse(VALID).unwrap();
        cfg.override_poll_interval(None);
        assert_eq!(cfg.poll_interval, Duration::from_secs(5));
    }

    #[test]
    fn build_adapters_succeeds_for_valid_config() {
        let cfg = parse(VALID).unwrap();
        let adapters = build_adapters(&cfg).expect("adapter construction should not fail");
        assert_eq!(adapters.len(), 3);
        // chain_meta() must round-trip the configured id (safe: `new` and
        // `chain_meta` are not `todo!()`, only fetch_* are).
        assert_eq!(adapters[0].1.chain_meta().id, "monero-main");
    }

    #[test]
    fn peek_log_file_expands_tilde() {
        let path = peek_log_file(VALID).unwrap();
        assert!(!path.to_string_lossy().starts_with('~'));
        assert!(path
            .to_string_lossy()
            .ends_with(".local/state/poolwatch/poolwatch.log"));
    }

    #[test]
    fn matches_real_example_file() {
        // Guards against config.rs's schema silently drifting from the
        // repo's documented example (which this crate must not modify).
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../poolwatch.example.toml"
        ))
        .expect("poolwatch.example.toml should exist at the repo root");
        let cfg = parse(&text).expect("the example file itself must be valid");
        assert_eq!(cfg.chains.len(), 4);
    }
}
