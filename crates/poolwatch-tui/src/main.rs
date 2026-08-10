//! poolwatch -- "htop for your mempool": a terminal UI that monitors the
//! mempools of multiple blockchain daemons (Monero/Oxen/Beldex/Bitcoin
//! families) over their node RPC.
//!
//! This binary wires together the sibling `poolwatch-core` (domain model,
//! tracking, stats) and `poolwatch-adapters` (concrete RPC clients) crates
//! behind a ratatui TUI, plus a scriptable `--once` report mode.

mod app;
mod config;
mod poller;
mod ui;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use clap::Parser;
use poolwatch_core::stats::fee_bands;
use poolwatch_core::MempoolSnapshot;
use ratatui::crossterm::event::Event as CtEvent;
use tokio::sync::mpsc;

#[derive(Debug, Parser)]
#[command(name = "poolwatch", version, about = "htop for your mempool")]
struct Cli {
    /// Path to the config file.
    #[arg(long, default_value = "poolwatch.toml")]
    config: PathBuf,

    /// Restrict to this chain id (repeatable). Default: every configured chain.
    #[arg(long = "chain")]
    chain: Vec<String>,

    /// Override poll_interval_secs from the config file.
    #[arg(long)]
    interval: Option<u64>,

    /// Fetch one snapshot per chain, print a plain-text report, and exit
    /// (no TUI). Exits 0 if at least one chain succeeded, else 1.
    #[arg(long)]
    once: bool,
}

fn main() {
    if let Err(err) = run() {
        // Only reachable for pre-TUI setup errors (config, CLI, logging) or
        // post-TUI errors surfaced after the terminal was already restored
        // -- never while the alternate screen owns the terminal.
        eprintln!("poolwatch: {err:?}");
        std::process::exit(1);
    }
}

/// Runs Tokio-driven setup and dispatch. Kept synchronous at the very top
/// (`main`) so the process can exit with a plain, un-mangled error message
/// before any async runtime or terminal state exists.
#[tokio::main]
async fn run() -> Result<()> {
    let cli = Cli::parse();

    let text = config::read_config_text(&cli.config)?;
    let log_path = config::peek_log_file(&text)?;
    init_logging(&log_path)?;

    let mut cfg = config::parse(&text)?;
    cfg.override_poll_interval(cli.interval);
    cfg.restrict_to_chains(&cli.chain)?;

    tracing::info!(
        chains = cfg.chains.len(),
        poll_interval_secs = cfg.poll_interval.as_secs(),
        stats_interval_secs = cfg.stats_interval.as_secs(),
        log_file = %cfg.log_file.display(),
        once = cli.once,
        "poolwatch starting"
    );

    if cli.once {
        let any_ok = run_once(&cfg).await?;
        std::process::exit(if any_ok { 0 } else { 1 });
    }

    run_tui(cfg).await
}

fn init_logging(log_path: &Path) -> Result<()> {
    if let Some(parent) = log_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create log directory '{}'", parent.display())
            })?;
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("failed to open log file '{}'", log_path.display()))?;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_writer(std::sync::Mutex::new(file))
        .with_ansi(false)
        .with_env_filter(filter)
        .init();

    Ok(())
}

// ---------------------------------------------------------------------
// --once mode: no TUI, a plain-text report on stdout.
// ---------------------------------------------------------------------

struct OnceReport {
    chain_id: String,
    ok: bool,
    text: String,
}

async fn run_once(cfg: &config::RuntimeConfig) -> Result<bool> {
    let adapters = config::build_adapters(cfg).context("failed to construct adapters")?;

    let mut set = tokio::task::JoinSet::new();
    for (chain_cfg, adapter) in adapters {
        set.spawn(async move {
            let outcome =
                match tokio::time::timeout(Duration::from_secs(10), adapter.fetch_snapshot()).await
                {
                    Ok(Ok(snapshot)) => Ok(snapshot),
                    Ok(Err(err)) => Err(err.to_string()),
                    Err(_elapsed) => Err("timed out after 10s".to_string()),
                };
            (chain_cfg, outcome)
        });
    }

    let mut reports = Vec::new();
    while let Some(joined) = set.join_next().await {
        let (chain_cfg, outcome) = joined.context("chain report task panicked")?;
        reports.push(format_once_report(&chain_cfg, outcome));
    }
    // Task completion order is nondeterministic; print in a stable order.
    reports.sort_by(|a, b| a.chain_id.cmp(&b.chain_id));

    let mut any_ok = false;
    for report in &reports {
        any_ok |= report.ok;
        println!("{}", report.text);
    }
    Ok(any_ok)
}

fn format_once_report(
    cfg: &config::ChainRuntimeCfg,
    outcome: std::result::Result<MempoolSnapshot, String>,
) -> OnceReport {
    let header = format!("{} ({})", cfg.meta.display_name, cfg.meta.id);
    match outcome {
        Ok(snapshot) => {
            let bands = fee_bands(&snapshot.txs);
            let top_line = match bands.last() {
                Some(b) => format!(
                    "top fee band: {} {} ({} tx)",
                    b.label,
                    ui::fmt_fee_rate(b.fee_per_byte, &cfg.meta.fee_rate_unit),
                    b.tx_count
                ),
                None => "top fee band: n/a (empty pool)".to_string(),
            };
            let text = format!(
                "{header}: OK\n  txs: {}\n  pool size: {}\n  height: {}\n  {top_line}",
                snapshot.txs.len(),
                ui::humanize_bytes(snapshot.pool_bytes),
                snapshot.node_height,
            );
            OnceReport {
                chain_id: cfg.meta.id.clone(),
                ok: true,
                text,
            }
        }
        Err(err) => OnceReport {
            chain_id: cfg.meta.id.clone(),
            ok: false,
            text: format!("{header}: ERROR - {err}"),
        },
    }
}

// ---------------------------------------------------------------------
// TUI mode.
// ---------------------------------------------------------------------

/// Restores the terminal on drop, whether the TUI loop exits normally or
/// via an early `?`. `ratatui::try_init` already installs a panic hook that
/// restores the terminal before the panic message prints, so no separate
/// hook is needed for the panic case.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

async fn run_tui(cfg: config::RuntimeConfig) -> Result<()> {
    let adapters = config::build_adapters(&cfg).context("failed to construct adapters")?;
    let chain_cfgs: Vec<_> = adapters.iter().map(|(c, _)| c.clone()).collect();
    let adapter_handles: Vec<_> = adapters.into_iter().map(|(_, a)| a).collect();

    let (chain_rx, poller) = poller::spawn(adapter_handles, cfg.poll_interval, cfg.stats_interval);

    let mut terminal = ratatui::try_init().context("failed to initialize terminal")?;
    let _guard = TerminalGuard;

    // Tail expression (no `?`): `_guard` (and `terminal`) drop right here,
    // restoring the terminal, before this function returns to `run()` --
    // whether the event loop finished cleanly or with an error.
    run_event_loop(
        &mut terminal,
        &chain_cfgs,
        cfg.stats_interval,
        chain_rx,
        poller,
    )
    .await
}

async fn run_event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    chain_cfgs: &[config::ChainRuntimeCfg],
    stats_interval: Duration,
    mut chain_rx: mpsc::Receiver<poller::ChainEvent>,
    poller: poller::PollerHandles,
) -> Result<()> {
    let poller::PollerHandles {
        cancel,
        paused,
        tasks,
    } = poller;

    let mut app = app::App::new(chain_cfgs, stats_interval, paused);

    let stop_input = Arc::new(AtomicBool::new(false));
    let mut input_rx = spawn_input_thread(Arc::clone(&stop_input));

    let mut redraw_tick = tokio::time::interval(Duration::from_millis(250));
    redraw_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    terminal.draw(|f| ui::draw(f, &mut app))?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                app.should_quit = true;
            }
            event = chain_rx.recv() => {
                if let Some(event) = event {
                    app.apply_chain_event(event, SystemTime::now());
                }
            }
            event = input_rx.recv() => {
                if let Some(event) = event {
                    app.handle_crossterm_event(event);
                }
            }
            _ = redraw_tick.tick() => {
                terminal.draw(|f| ui::draw(f, &mut app))?;
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Clean shutdown: stop the input thread's polling loop (best effort;
    // it's a detached OS thread, see `spawn_input_thread`), cancel every
    // poller task, and wait for them to actually finish.
    stop_input.store(true, Ordering::Relaxed);
    cancel.cancel();
    for task in tasks {
        let _ = task.await;
    }

    Ok(())
}

/// Reads crossterm input events on a dedicated OS thread and forwards them
/// over an unbounded channel.
///
/// Why a thread instead of `crossterm::event::EventStream`: that type
/// requires crossterm's `event-stream` cargo feature, which is not enabled
/// anywhere in this workspace's dependency graph (ratatui's bundled
/// crossterm only turns on its default features). Adding a direct
/// `crossterm` dependency here to request that feature is exactly what the
/// task spec forbids ("never a separate crossterm dep -- version mismatch
/// breaks the build"), since it would pull in a second, independently
/// resolved copy of the crate. Polling with a short timeout on a plain OS
/// thread needs no extra feature and stays within `ratatui::crossterm`.
///
/// The thread is deliberately not joined on shutdown: `event::read()` is a
/// blocking call on stdin with no async cancellation hook, so joining it
/// could hang shutdown for up to a keypress. `stop` is set so the thread
/// exits within one poll timeout if the process outlives the TUI loop; if
/// the process exits first, the OS reclaims the thread.
fn spawn_input_thread(stop: Arc<AtomicBool>) -> mpsc::UnboundedReceiver<CtEvent> {
    use ratatui::crossterm::event as ct_event;

    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("poolwatch-input".to_string())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match ct_event::poll(Duration::from_millis(200)) {
                    Ok(true) => match ct_event::read() {
                        Ok(ev) => {
                            if tx.send(ev).is_err() {
                                return; // receiver gone: app is shutting down
                            }
                        }
                        Err(_) => return,
                    },
                    Ok(false) => {} // timeout: loop back and recheck `stop`
                    Err(_) => return,
                }
            }
        })
        .expect("failed to spawn input reader thread");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_defaults() {
        let cli = Cli::try_parse_from(["poolwatch"]).unwrap();
        assert_eq!(cli.config, PathBuf::from("poolwatch.toml"));
        assert!(cli.chain.is_empty());
        assert_eq!(cli.interval, None);
        assert!(!cli.once);
    }

    #[test]
    fn cli_parses_repeated_chain_flags() {
        let cli = Cli::try_parse_from([
            "poolwatch",
            "--chain",
            "monero-main",
            "--chain",
            "bitcoin-main",
        ])
        .unwrap();
        assert_eq!(cli.chain, vec!["monero-main", "bitcoin-main"]);
    }

    #[test]
    fn cli_parses_config_interval_and_once() {
        let cli = Cli::try_parse_from([
            "poolwatch",
            "--config",
            "/tmp/custom.toml",
            "--interval",
            "30",
            "--once",
        ])
        .unwrap();
        assert_eq!(cli.config, PathBuf::from("/tmp/custom.toml"));
        assert_eq!(cli.interval, Some(30));
        assert!(cli.once);
    }

    #[test]
    fn cli_rejects_unknown_flags() {
        assert!(Cli::try_parse_from(["poolwatch", "--not-a-flag"]).is_err());
    }

    fn test_chain_cfg() -> config::ChainRuntimeCfg {
        config::ChainRuntimeCfg {
            meta: poolwatch_core::ChainMeta {
                id: "monero-main".to_string(),
                display_name: "Monero".to_string(),
                ticker: "XMR".to_string(),
                decimals: 12,
                fee_rate_unit: "au/B".to_string(),
            },
            rpc_url: "http://127.0.0.1:18081".to_string(),
            max_pool_response_bytes: 1024,
            family: config::ChainFamilyCfg::Cryptonote {
                rpc_login: None,
                instant_tx: None,
            },
        }
    }

    // Only the Err branch is safe to unit test: the Ok branch calls
    // `stats::fee_bands`, which is `todo!()` in the frozen core crate
    // today (see the task contract) even for an empty tx slice.
    #[test]
    fn format_once_report_error_branch() {
        let report = format_once_report(&test_chain_cfg(), Err("connection refused".to_string()));
        assert!(!report.ok);
        assert_eq!(report.chain_id, "monero-main");
        assert!(report.text.contains("Monero (monero-main): ERROR"));
        assert!(report.text.contains("connection refused"));
    }
}
