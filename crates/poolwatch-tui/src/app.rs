//! Application state: per-chain UI state, chain-event application, and key
//! handling. Rendering itself lives in `ui/*`; this module owns *what* is
//! true, not how it looks on screen.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use poolwatch_core::stats::TimeSeries;
use poolwatch_core::{ChainMeta, ChainTracker, MempoolSnapshot, PoolStats};
use ratatui::crossterm::event::{Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::config::ChainRuntimeCfg;
use crate::poller::{ChainEvent, ChainPayload};
use crate::ui::tx_table::{SortKey, TxTableState};

/// Which top-level screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Dashboard,
    Detail,
}

/// Per-chain reachability, derived from the poller's event stream.
///
/// `Snapshot` (full fetch success), `Stats` (cheap fetch success), and
/// `Degraded` (too-large/unsupported) are all positive evidence the chain
/// is reachable, so each of them clears `Unreachable`. Only a full
/// `Snapshot` success clears `Degraded` -- a successful cheap stats call
/// during stats-only mode doesn't prove the full-fetch problem is gone.
#[derive(Debug, Clone)]
pub enum ChainStatus {
    Ok,
    Degraded { reason: String },
    Unreachable { error: String, retry_at: Instant },
}

/// Everything the UI needs to know about one configured chain.
pub struct ChainState {
    pub meta: ChainMeta,
    pub is_cryptonote: bool,
    pub tracker: ChainTracker,
    pub latest_snapshot: Option<MempoolSnapshot>,
    pub latest_stats: Option<PoolStats>,
    /// Sampled at stats cadence; each holds ~15 minutes of history.
    pub tx_count_series: TimeSeries,
    pub pool_bytes_series: TimeSeries,
    pub inflow_series: TimeSeries,
    pub status: ChainStatus,
    pub last_update: Option<SystemTime>,
    pub tx_table: TxTableState,
}

impl ChainState {
    fn new(cfg: &ChainRuntimeCfg, stats_interval: Duration) -> Self {
        // 15 minutes of history at stats cadence.
        let secs = stats_interval.as_secs().max(1);
        let cap = ((15 * 60) / secs).max(1) as usize;
        Self {
            meta: cfg.meta.clone(),
            is_cryptonote: cfg.is_cryptonote(),
            tracker: ChainTracker::new(),
            latest_snapshot: None,
            latest_stats: None,
            tx_count_series: TimeSeries::new(cap),
            pool_bytes_series: TimeSeries::new(cap),
            inflow_series: TimeSeries::new(cap),
            status: ChainStatus::Ok,
            last_update: None,
            tx_table: TxTableState::default(),
        }
    }

    /// A successful cheap call proves the chain is reachable: clears
    /// `Unreachable`, but leaves `Degraded` alone.
    fn note_reachable(&mut self) {
        if matches!(self.status, ChainStatus::Unreachable { .. }) {
            self.status = ChainStatus::Ok;
        }
    }

    fn apply_payload(&mut self, payload: ChainPayload, now: SystemTime) {
        self.last_update = Some(now);
        match payload {
            ChainPayload::Snapshot(mut snapshot) => {
                self.tracker.apply(&mut snapshot);
                self.latest_snapshot = Some(snapshot);
                self.status = ChainStatus::Ok;
            }
            ChainPayload::Stats(stats) => {
                let derived = self.tracker.stats(stats.taken_at);
                self.tx_count_series
                    .push(stats.taken_at, stats.tx_count as f64);
                self.pool_bytes_series
                    .push(stats.taken_at, stats.pool_bytes as f64);
                self.inflow_series.push(stats.taken_at, derived.inflow_1m);
                self.latest_stats = Some(stats);
                self.note_reachable();
            }
            ChainPayload::Degraded { reason } => {
                self.status = ChainStatus::Degraded { reason };
            }
            ChainPayload::Failed {
                error,
                next_retry_secs,
            } => {
                self.status = ChainStatus::Unreachable {
                    error,
                    retry_at: Instant::now() + Duration::from_secs(next_retry_secs),
                };
            }
        }
    }
}

/// Top-level application state.
pub struct App {
    pub chains: Vec<ChainState>,
    pub view: View,
    pub selected: usize,
    pub show_help: bool,
    pub paused: Arc<AtomicBool>,
    pub should_quit: bool,
}

impl App {
    pub fn new(
        configs: &[ChainRuntimeCfg],
        stats_interval: Duration,
        paused: Arc<AtomicBool>,
    ) -> Self {
        let chains = configs
            .iter()
            .map(|c| ChainState::new(c, stats_interval))
            .collect();
        Self {
            chains,
            view: View::Dashboard,
            selected: 0,
            show_help: false,
            paused,
            should_quit: false,
        }
    }

    pub fn apply_chain_event(&mut self, event: ChainEvent, now: SystemTime) {
        if let Some(chain) = self.chains.iter_mut().find(|c| c.meta.id == event.chain_id) {
            chain.apply_payload(event.payload, now);
        }
    }

    pub fn selected_chain(&self) -> Option<&ChainState> {
        self.chains.get(self.selected)
    }

    pub fn selected_chain_mut(&mut self) -> Option<&mut ChainState> {
        self.chains.get_mut(self.selected)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.chains.is_empty() {
            return;
        }
        let len = self.chains.len() as isize;
        let cur = self.selected as isize;
        let next = ((cur + delta) % len + len) % len;
        self.selected = next as usize;
    }

    /// Handles one crossterm input event; only key-press events matter.
    pub fn handle_crossterm_event(&mut self, event: CtEvent) {
        let CtEvent::Key(key) = event else { return };
        if key.kind != KeyEventKind::Press {
            return; // ignore repeat/release (mainly a Windows concern)
        }
        self.handle_key(key);
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // Ctrl-C always quits, even mid-filter-edit or under the help
        // overlay -- checked first and unconditionally.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }

        // A chain's txid filter prompt, when open, owns the keyboard: every
        // other binding below would otherwise be swallowed into the filter
        // text, which is exactly what we want while typing.
        if self.filter_editing() {
            self.handle_filter_key(key);
            return;
        }

        if self.show_help {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc => self.show_help = false,
                KeyCode::Char('q') => self.should_quit = true,
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('p') => {
                self.paused.fetch_xor(true, Ordering::Relaxed);
            }
            KeyCode::Char('d') => self.view = View::Dashboard,
            KeyCode::Tab => {
                self.move_selection(1);
                self.view = View::Detail;
            }
            KeyCode::Enter => {
                if !self.chains.is_empty() {
                    self.view = View::Detail;
                }
            }
            KeyCode::Esc => {
                if self.view == View::Detail {
                    self.view = View::Dashboard;
                }
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as u8 - b'1') as usize;
                if idx < self.chains.len() {
                    self.selected = idx;
                    self.view = View::Detail;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_up(),
            KeyCode::Down | KeyCode::Char('j') => self.move_down(),
            KeyCode::Char('f') if self.view == View::Detail => self.set_sort(SortKey::Fee),
            KeyCode::Char('s') if self.view == View::Detail => self.set_sort(SortKey::Size),
            KeyCode::Char('t') if self.view == View::Detail => self.set_sort(SortKey::Age),
            KeyCode::Char('/') if self.view == View::Detail => {
                if let Some(chain) = self.selected_chain_mut() {
                    chain.tx_table.start_editing();
                }
            }
            _ => {}
        }
    }

    fn set_sort(&mut self, key: SortKey) {
        if let Some(chain) = self.selected_chain_mut() {
            chain.tx_table.sort = key;
        }
    }

    fn move_up(&mut self) {
        match self.view {
            View::Dashboard => self.move_selection(-1),
            View::Detail => {
                if let Some(chain) = self.selected_chain_mut() {
                    chain.tx_table.table.select_previous();
                }
            }
        }
    }

    fn move_down(&mut self) {
        match self.view {
            View::Dashboard => self.move_selection(1),
            View::Detail => {
                if let Some(chain) = self.selected_chain_mut() {
                    chain.tx_table.table.select_next();
                }
            }
        }
    }

    fn filter_editing(&self) -> bool {
        self.selected_chain()
            .is_some_and(|c| c.tx_table.filter_editing)
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        let Some(chain) = self.selected_chain_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => chain.tx_table.clear_filter(),
            KeyCode::Enter => chain.tx_table.confirm_editing(),
            KeyCode::Backspace => chain.tx_table.backspace_filter(),
            KeyCode::Char(c) => chain.tx_table.push_filter_char(c),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use poolwatch_core::FeeBand;
    use ratatui::crossterm::event::KeyEventState;

    fn meta(id: &str) -> ChainMeta {
        ChainMeta {
            id: id.to_string(),
            display_name: id.to_string(),
            ticker: "T".to_string(),
            decimals: 8,
            fee_rate_unit: "u/B".to_string(),
        }
    }

    fn cfg(id: &str) -> ChainRuntimeCfg {
        ChainRuntimeCfg {
            meta: meta(id),
            rpc_url: "http://127.0.0.1:1".to_string(),
            max_pool_response_bytes: 1024,
            family: crate::config::ChainFamilyCfg::Cryptonote {
                rpc_login: None,
                instant_tx: None,
            },
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn app_with_chains(ids: &[&str]) -> App {
        let cfgs: Vec<_> = ids.iter().map(|id| cfg(id)).collect();
        App::new(
            &cfgs,
            Duration::from_secs(2),
            Arc::new(AtomicBool::new(false)),
        )
    }

    #[test]
    fn new_app_starts_on_dashboard_unselected_help_closed() {
        let app = app_with_chains(&["a", "b"]);
        assert_eq!(app.view, View::Dashboard);
        assert_eq!(app.selected, 0);
        assert!(!app.show_help);
        assert!(!app.should_quit);
        assert_eq!(app.chains.len(), 2);
    }

    #[test]
    fn q_quits() {
        let mut app = app_with_chains(&["a"]);
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = app_with_chains(&["a"]);
        app.handle_key(ctrl_key(KeyCode::Char('c')));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_quits_even_while_filter_editing() {
        let mut app = app_with_chains(&["a"]);
        app.view = View::Detail;
        app.selected_chain_mut().unwrap().tx_table.start_editing();
        app.handle_key(ctrl_key(KeyCode::Char('c')));
        assert!(app.should_quit);
    }

    #[test]
    fn digit_jumps_to_chain_detail() {
        let mut app = app_with_chains(&["a", "b", "c"]);
        app.handle_key(key(KeyCode::Char('2')));
        assert_eq!(app.view, View::Detail);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn digit_out_of_range_is_ignored() {
        let mut app = app_with_chains(&["a"]);
        app.handle_key(key(KeyCode::Char('5')));
        assert_eq!(app.view, View::Dashboard);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn tab_cycles_chains_and_opens_detail() {
        let mut app = app_with_chains(&["a", "b", "c"]);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.selected, 1);
        assert_eq!(app.view, View::Detail);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.selected, 2);
        app.handle_key(key(KeyCode::Tab)); // wraps
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn d_returns_to_dashboard() {
        let mut app = app_with_chains(&["a"]);
        app.view = View::Detail;
        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.view, View::Dashboard);
    }

    #[test]
    fn esc_returns_to_dashboard_from_detail() {
        let mut app = app_with_chains(&["a"]);
        app.view = View::Detail;
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.view, View::Dashboard);
    }

    #[test]
    fn up_down_move_dashboard_selection_with_wraparound() {
        let mut app = app_with_chains(&["a", "b", "c"]);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.selected, 1);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, 2);
        app.handle_key(key(KeyCode::Down)); // wraps to 0
        assert_eq!(app.selected, 0);
        app.handle_key(key(KeyCode::Up)); // wraps back to last
        assert_eq!(app.selected, 2);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn sort_keys_only_apply_in_detail_view() {
        let mut app = app_with_chains(&["a"]);
        app.handle_key(key(KeyCode::Char('s'))); // dashboard: no-op
        assert_eq!(app.selected_chain().unwrap().tx_table.sort, SortKey::Fee);

        app.view = View::Detail;
        app.handle_key(key(KeyCode::Char('s')));
        assert_eq!(app.selected_chain().unwrap().tx_table.sort, SortKey::Size);
        app.handle_key(key(KeyCode::Char('t')));
        assert_eq!(app.selected_chain().unwrap().tx_table.sort, SortKey::Age);
        app.handle_key(key(KeyCode::Char('f')));
        assert_eq!(app.selected_chain().unwrap().tx_table.sort, SortKey::Fee);
    }

    #[test]
    fn slash_opens_filter_only_in_detail_view() {
        let mut app = app_with_chains(&["a"]);
        app.handle_key(key(KeyCode::Char('/')));
        assert!(!app.selected_chain().unwrap().tx_table.filter_editing);

        app.view = View::Detail;
        app.handle_key(key(KeyCode::Char('/')));
        assert!(app.selected_chain().unwrap().tx_table.filter_editing);
    }

    #[test]
    fn filter_editing_captures_letters_that_are_also_global_bindings() {
        let mut app = app_with_chains(&["a"]);
        app.view = View::Detail;
        app.selected_chain_mut().unwrap().tx_table.start_editing();

        // 'q', 'f', 'p', '?' etc. must all be typed into the filter, not
        // trigger their global actions, while the prompt is open.
        for c in ['q', 'f', 's', 't', 'p', '?', 'd'] {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert!(!app.should_quit);
        assert!(!app.show_help);
        assert_eq!(app.selected_chain().unwrap().tx_table.sort, SortKey::Fee);
        assert_eq!(app.selected_chain().unwrap().tx_table.filter, "qfstp?d");

        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.selected_chain().unwrap().tx_table.filter, "");
        assert!(!app.selected_chain().unwrap().tx_table.filter_editing);
    }

    #[test]
    fn filter_enter_confirms_and_keeps_text() {
        let mut app = app_with_chains(&["a"]);
        app.view = View::Detail;
        app.selected_chain_mut().unwrap().tx_table.start_editing();
        app.handle_key(key(KeyCode::Char('a')));
        app.handle_key(key(KeyCode::Char('b')));
        app.handle_key(key(KeyCode::Enter));
        let table = &app.selected_chain().unwrap().tx_table;
        assert!(!table.filter_editing);
        assert_eq!(table.filter, "ab");
    }

    #[test]
    fn p_toggles_global_pause_flag() {
        let mut app = app_with_chains(&["a"]);
        assert!(!app.paused.load(Ordering::Relaxed));
        app.handle_key(key(KeyCode::Char('p')));
        assert!(app.paused.load(Ordering::Relaxed));
        app.handle_key(key(KeyCode::Char('p')));
        assert!(!app.paused.load(Ordering::Relaxed));
    }

    #[test]
    fn help_overlay_swallows_keys_except_close_and_quit() {
        let mut app = app_with_chains(&["a"]);
        app.handle_key(key(KeyCode::Char('?')));
        assert!(app.show_help);

        app.handle_key(key(KeyCode::Char('d'))); // swallowed
        assert!(app.show_help);
        assert_eq!(app.view, View::Dashboard);

        app.handle_key(key(KeyCode::Esc));
        assert!(!app.show_help);
    }

    #[test]
    fn stats_event_clears_unreachable_but_not_degraded() {
        let mut app = app_with_chains(&["a"]);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);

        app.apply_chain_event(
            ChainEvent {
                chain_id: "a".to_string(),
                payload: ChainPayload::Failed {
                    error: "boom".to_string(),
                    next_retry_secs: 1,
                },
            },
            now,
        );
        assert!(matches!(
            app.selected_chain().unwrap().status,
            ChainStatus::Unreachable { .. }
        ));

        app.apply_chain_event(
            ChainEvent {
                chain_id: "a".to_string(),
                payload: ChainPayload::Stats(PoolStats {
                    chain_id: "a".to_string(),
                    taken_at: now,
                    tx_count: 3,
                    pool_bytes: 300,
                    fee_total: None,
                    double_spends: None,
                    oldest: None,
                }),
            },
            now,
        );
        assert!(matches!(
            app.selected_chain().unwrap().status,
            ChainStatus::Ok
        ));

        app.apply_chain_event(
            ChainEvent {
                chain_id: "a".to_string(),
                payload: ChainPayload::Degraded {
                    reason: "too big".to_string(),
                },
            },
            now,
        );
        assert!(matches!(
            app.selected_chain().unwrap().status,
            ChainStatus::Degraded { .. }
        ));

        // A further Stats success must NOT clear Degraded.
        app.apply_chain_event(
            ChainEvent {
                chain_id: "a".to_string(),
                payload: ChainPayload::Stats(PoolStats {
                    chain_id: "a".to_string(),
                    taken_at: now,
                    tx_count: 4,
                    pool_bytes: 400,
                    fee_total: None,
                    double_spends: None,
                    oldest: None,
                }),
            },
            now,
        );
        assert!(matches!(
            app.selected_chain().unwrap().status,
            ChainStatus::Degraded { .. }
        ));
    }

    #[test]
    fn unknown_chain_id_event_is_ignored_without_panicking() {
        let mut app = app_with_chains(&["a"]);
        app.apply_chain_event(
            ChainEvent {
                chain_id: "does-not-exist".to_string(),
                payload: ChainPayload::Degraded {
                    reason: "x".to_string(),
                },
            },
            SystemTime::now(),
        );
        assert!(matches!(
            app.selected_chain().unwrap().status,
            ChainStatus::Ok
        ));
    }

    // Exercises FeeBand's shape even though `stats::fee_bands` itself is
    // `todo!()` today; keeps this test file honest about what it imports.
    #[test]
    fn fee_band_type_is_constructible() {
        let band = FeeBand {
            label: "p50".to_string(),
            fee_per_byte: 1.5,
            tx_count: 2,
        };
        assert_eq!(band.label, "p50");
    }
}
