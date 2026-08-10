//! Scrollable, sortable, filterable transaction table (bottom panel of the
//! chain detail view).
//!
//! The sort/filter logic is written as pure functions over `&[PoolTx]` so
//! it can be unit-tested with synthetic transactions, independent of any
//! rendering or the (currently `todo!()`) tracker/stats/adapter code.

use std::time::SystemTime;

use poolwatch_core::{InstantTxKind, PoolTx};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Cell, Row, Table, TableState};
use ratatui::Frame;

use crate::ui::{fmt_fee_rate, humanize_bytes, humanize_secs_ago, short_txid};

/// Active sort column. Fee (descending) is the default per spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Fee,
    Size,
    Age,
}

/// Per-chain UI state for the tx table: sort key, txid-prefix filter (and
/// whether the filter prompt is being edited), and row selection/scroll.
#[derive(Debug, Default)]
pub struct TxTableState {
    pub sort: SortKey,
    pub filter: String,
    pub filter_editing: bool,
    pub table: TableState,
}

impl TxTableState {
    pub fn start_editing(&mut self) {
        self.filter_editing = true;
    }

    pub fn confirm_editing(&mut self) {
        self.filter_editing = false;
    }

    /// Esc: clears the filter and closes the prompt, per spec.
    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.filter_editing = false;
    }

    pub fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
    }

    pub fn backspace_filter(&mut self) {
        self.filter.pop();
    }
}

/// Filters txs whose txid starts with `prefix`, case-insensitively. An
/// empty prefix matches everything.
pub fn filter_txs<'a>(txs: &'a [PoolTx], prefix: &str) -> Vec<&'a PoolTx> {
    if prefix.is_empty() {
        return txs.iter().collect();
    }
    let needle = prefix.to_ascii_lowercase();
    txs.iter()
        .filter(|t| t.txid.to_ascii_lowercase().starts_with(&needle))
        .collect()
}

/// Sorts tx references by `key`. Fee and size are descending (largest
/// first); age is ascending (oldest -- earliest `first_seen_local` --
/// first).
pub fn sort_txs(mut txs: Vec<&PoolTx>, key: SortKey) -> Vec<&PoolTx> {
    match key {
        // f64 has no total Ord, so this one can't become sort_by_key.
        SortKey::Fee => txs.sort_by(|a, b| b.fee_per_byte.total_cmp(&a.fee_per_byte)),
        SortKey::Size => txs.sort_by_key(|t| std::cmp::Reverse(t.size_bytes)),
        SortKey::Age => txs.sort_by_key(|t| t.first_seen_local),
    }
    txs
}

/// Combined filter + sort: the order the table actually renders rows in.
pub fn visible_rows<'a>(txs: &'a [PoolTx], filter: &str, sort: SortKey) -> Vec<&'a PoolTx> {
    sort_txs(filter_txs(txs, filter), sort)
}

fn flags(tx: &PoolTx) -> &'static str {
    match (tx.instant, tx.double_spend_seen) {
        (InstantTxKind::None, false) => "",
        (InstantTxKind::None, true) => "\u{26a0}",
        (_, false) => "\u{26a1}",
        (_, true) => "\u{26a1}\u{26a0}",
    }
}

/// Renders the tx table into `area`, mutating `state` only to keep the
/// selection index in range as the filtered/sorted row set changes size.
#[allow(clippy::too_many_arguments)]
pub fn render(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    txs: &[PoolTx],
    fee_rate_unit: &str,
    now: SystemTime,
    state: &mut TxTableState,
) {
    let rows = visible_rows(txs, &state.filter, state.sort);

    if rows.is_empty() {
        state.table.select(None);
    } else {
        let selected = state.table.selected().unwrap_or(0).min(rows.len() - 1);
        state.table.select(Some(selected));
    }

    let header = Row::new(vec![
        header_cell("TXID", None, state.sort),
        header_cell("SIZE", Some(SortKey::Size), state.sort),
        header_cell("FEE/B", Some(SortKey::Fee), state.sort),
        header_cell("AGE", Some(SortKey::Age), state.sort),
        header_cell("FLAGS", None, state.sort),
    ]);

    let body_rows: Vec<Row> = rows
        .iter()
        .map(|tx| {
            let age = now.duration_since(tx.first_seen_local).unwrap_or_default();
            Row::new(vec![
                Cell::from(short_txid(&tx.txid)),
                Cell::from(humanize_bytes(tx.size_bytes)),
                Cell::from(fmt_fee_rate(tx.fee_per_byte, fee_rate_unit)),
                Cell::from(humanize_secs_ago(age)),
                Cell::from(flags(tx)),
            ])
        })
        .collect();

    let filter_suffix = if state.filter_editing {
        format!(" /{}_", state.filter)
    } else if !state.filter.is_empty() {
        format!(" [/{}]", state.filter)
    } else {
        String::new()
    };
    let block = Block::bordered().title(format!(
        "{title} ({}/{}){filter_suffix}",
        rows.len(),
        txs.len()
    ));

    let widths = [
        Constraint::Length(18),
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Length(10),
        Constraint::Length(6),
    ];

    let table = Table::new(body_rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");

    frame.render_stateful_widget(table, area, &mut state.table);
}

fn header_cell(label: &str, key: Option<SortKey>, active: SortKey) -> Cell<'static> {
    let text = match key {
        Some(k) if k == active => format!("{label} \u{25bc}"),
        _ => label.to_string(),
    };
    Cell::from(text).style(Style::default().add_modifier(Modifier::BOLD))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tx(txid: &str, size_bytes: u64, fee_per_byte: f64, first_seen_offset_secs: u64) -> PoolTx {
        PoolTx {
            txid: txid.to_string(),
            size_bytes,
            fee_atomic: (fee_per_byte * size_bytes as f64) as u128,
            fee_per_byte,
            received_by_node: None,
            first_seen_local: SystemTime::UNIX_EPOCH + Duration::from_secs(first_seen_offset_secs),
            instant: InstantTxKind::None,
            in_out_counts: None,
            double_spend_seen: false,
        }
    }

    #[test]
    fn filter_empty_matches_all() {
        let txs = vec![tx("abc123", 100, 1.0, 0), tx("def456", 100, 1.0, 0)];
        assert_eq!(filter_txs(&txs, "").len(), 2);
    }

    #[test]
    fn filter_matches_prefix_case_insensitively() {
        let txs = vec![tx("ABCDEF", 100, 1.0, 0), tx("zzzzzz", 100, 1.0, 0)];
        let filtered = filter_txs(&txs, "abc");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].txid, "ABCDEF");
    }

    #[test]
    fn filter_excludes_non_matching() {
        let txs = vec![tx("abcdef", 100, 1.0, 0)];
        assert!(filter_txs(&txs, "zzz").is_empty());
    }

    #[test]
    fn sort_by_fee_is_descending() {
        let txs = [
            tx("a", 100, 1.0, 0),
            tx("b", 100, 5.0, 0),
            tx("c", 100, 3.0, 0),
        ];
        let refs: Vec<&PoolTx> = txs.iter().collect();
        let sorted = sort_txs(refs, SortKey::Fee);
        let ids: Vec<&str> = sorted.iter().map(|t| t.txid.as_str()).collect();
        assert_eq!(ids, vec!["b", "c", "a"]);
    }

    #[test]
    fn sort_by_size_is_descending() {
        let txs = [
            tx("a", 500, 1.0, 0),
            tx("b", 100, 1.0, 0),
            tx("c", 900, 1.0, 0),
        ];
        let refs: Vec<&PoolTx> = txs.iter().collect();
        let sorted = sort_txs(refs, SortKey::Size);
        let ids: Vec<&str> = sorted.iter().map(|t| t.txid.as_str()).collect();
        assert_eq!(ids, vec!["c", "a", "b"]);
    }

    #[test]
    fn sort_by_age_is_ascending_oldest_first() {
        let txs = [
            tx("newest", 100, 1.0, 300),
            tx("oldest", 100, 1.0, 10),
            tx("mid", 100, 1.0, 100),
        ];
        let refs: Vec<&PoolTx> = txs.iter().collect();
        let sorted = sort_txs(refs, SortKey::Age);
        let ids: Vec<&str> = sorted.iter().map(|t| t.txid.as_str()).collect();
        assert_eq!(ids, vec!["oldest", "mid", "newest"]);
    }

    #[test]
    fn visible_rows_combines_filter_then_sort() {
        let txs = vec![
            tx("aa_low", 100, 1.0, 0),
            tx("aa_high", 100, 9.0, 0),
            tx("bb_excluded", 100, 5.0, 0),
        ];
        let rows = visible_rows(&txs, "aa", SortKey::Fee);
        let ids: Vec<&str> = rows.iter().map(|t| t.txid.as_str()).collect();
        assert_eq!(ids, vec!["aa_high", "aa_low"]);
    }

    #[test]
    fn flags_reports_instant_and_double_spend() {
        let mut plain = tx("a", 100, 1.0, 0);
        assert_eq!(flags(&plain), "");

        plain.double_spend_seen = true;
        assert_eq!(flags(&plain), "\u{26a0}");

        let mut blink = tx("b", 100, 1.0, 0);
        blink.instant = InstantTxKind::Blink;
        assert_eq!(flags(&blink), "\u{26a1}");

        blink.double_spend_seen = true;
        assert_eq!(flags(&blink), "\u{26a1}\u{26a0}");
    }

    #[test]
    fn filter_state_transitions() {
        let mut state = TxTableState::default();
        assert!(!state.filter_editing);

        state.start_editing();
        assert!(state.filter_editing);

        state.push_filter_char('a');
        state.push_filter_char('b');
        assert_eq!(state.filter, "ab");

        state.backspace_filter();
        assert_eq!(state.filter, "a");

        state.confirm_editing();
        assert!(!state.filter_editing);
        assert_eq!(state.filter, "a"); // confirming keeps the text

        state.start_editing();
        state.clear_filter();
        assert!(!state.filter_editing);
        assert_eq!(state.filter, ""); // Esc clears
    }
}
