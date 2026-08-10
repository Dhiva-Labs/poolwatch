//! Dashboard view: one row per configured chain, plus an optional per-chain
//! sparkline strip (tx count, last 15m) when the terminal is tall enough.

use std::time::SystemTime;

use poolwatch_core::stats::fee_bands;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Cell, Row, Table};
use ratatui::Frame;

use crate::app::{App, ChainState};
use crate::ui::sparkline;
use crate::ui::{fmt_fee_rate, humanize_bytes, instant_summary, status_color};

/// Table chrome: top border + header row + bottom border, plus one row per
/// chain.
fn table_height(chain_count: u16) -> u16 {
    chain_count + 3
}

/// Sparkline strip chrome: top + bottom border, plus one row per chain.
fn sparkline_strip_height(chain_count: u16) -> u16 {
    chain_count + 2
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let now = SystemTime::now();
    let chain_count = app.chains.len() as u16;
    let table_h = table_height(chain_count);
    let strip_h = sparkline_strip_height(chain_count);

    let show_sparklines = area.height >= table_h + strip_h;

    let sections = if show_sparklines {
        Layout::vertical([Constraint::Length(table_h), Constraint::Min(0)]).split(area)
    } else {
        Layout::vertical([Constraint::Min(0)]).split(area)
    };

    render_table(frame, sections[0], app, now);

    if show_sparklines {
        render_sparkline_strip(frame, sections[1], app);
    }
}

fn render_table(frame: &mut Frame, area: Rect, app: &App, now: SystemTime) {
    let header = Row::new(vec![
        Cell::from(" "),
        Cell::from("CHAIN"),
        Cell::from("TXS"),
        Cell::from("POOL SIZE"),
        Cell::from("IN/MIN"),
        Cell::from("MED FEE/B"),
        Cell::from("INSTANT"),
        Cell::from("HEIGHT"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let selected = app.selected;
    let rows: Vec<Row> = app
        .chains
        .iter()
        .map(|chain| dashboard_row(chain, now))
        .enumerate()
        .map(|(i, row)| {
            if i == selected {
                row.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                row
            }
        })
        .collect();

    let widths = [
        Constraint::Length(1),
        Constraint::Length(14),
        Constraint::Length(6),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(15),
        Constraint::Length(14),
        Constraint::Length(10),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::bordered().title("chains"));

    frame.render_widget(table, area);
}

const MISSING: &str = "\u{2014}";

fn dashboard_row(chain: &ChainState, now: SystemTime) -> Row<'static> {
    let dot = Span::styled("\u{25cf}", Style::default().fg(status_color(&chain.status)));

    let (tx_count, pool_bytes) = chain
        .latest_stats
        .as_ref()
        .map(|s| (Some(s.tx_count), Some(s.pool_bytes)))
        .unwrap_or_else(|| {
            chain
                .latest_snapshot
                .as_ref()
                .map(|s| (Some(s.txs.len() as u64), Some(s.pool_bytes)))
                .unwrap_or((None, None))
        });

    let inflow = chain
        .last_update
        .map(|_| chain.tracker.stats(now).inflow_1m);

    let (med_fee, instant) = match &chain.latest_snapshot {
        Some(snap) => {
            let bands = fee_bands(&snap.txs);
            let med = bands
                .iter()
                .find(|b| b.label == "p50")
                .map(|b| fmt_fee_rate(b.fee_per_byte, &chain.meta.fee_rate_unit));
            (med, Some(instant_summary(&snap.txs)))
        }
        None => (None, None),
    };

    let height = chain.latest_snapshot.as_ref().map(|s| s.node_height);

    Row::new(vec![
        Cell::from(dot),
        Cell::from(chain.meta.display_name.clone()),
        Cell::from(tx_count.map_or_else(|| MISSING.to_string(), |v| v.to_string())),
        Cell::from(pool_bytes.map_or_else(|| MISSING.to_string(), humanize_bytes)),
        Cell::from(inflow.map_or_else(|| MISSING.to_string(), |v| format!("{v:.1}"))),
        Cell::from(med_fee.unwrap_or_else(|| MISSING.to_string())),
        Cell::from(instant.unwrap_or_else(|| MISSING.to_string())),
        Cell::from(height.map_or_else(|| MISSING.to_string(), |v| v.to_string())),
    ])
}

fn render_sparkline_strip(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered().title("tx count (last 15m)");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.chains.is_empty() {
        return;
    }

    let heights = vec![Constraint::Length(1); app.chains.len()];
    let rows = Layout::vertical(heights).split(inner);

    for (chain, row) in app.chains.iter().zip(rows.iter()) {
        let cols = Layout::horizontal([Constraint::Length(14), Constraint::Min(0)]).split(*row);
        frame.render_widget(Span::raw(chain.meta.display_name.clone()), cols[0]);
        sparkline::render_bare(
            frame,
            cols[1],
            &chain.tx_count_series,
            Style::default().fg(Color::Blue),
        );
    }
}
