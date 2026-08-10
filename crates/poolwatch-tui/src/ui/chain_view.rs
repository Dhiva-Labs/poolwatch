//! Chain detail view: fee bands (left), pool dynamics + sparklines (right),
//! and the tx table (bottom).
//!
//! Degrades gracefully on short terminals: sparklines in the dynamics
//! panel hide first (numbers stay), then -- on very short terminals -- the
//! tx table hides too, leaving just the dynamics summary.

use std::time::SystemTime;

use poolwatch_core::stats::fee_bands;
use poolwatch_core::PoolTx;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::app::ChainState;
use crate::ui::{fee_panel, humanize_secs_ago, sparkline, tx_table};

/// Below this height, sparklines in the dynamics panel are hidden.
const MIN_HEIGHT_FOR_SPARKLINES: u16 = 20;
/// Below this height, the tx table is hidden entirely.
const MIN_HEIGHT_FOR_TABLE: u16 = 12;
/// Used as the tx table's input before the first snapshot arrives.
const EMPTY_TXS: &[PoolTx] = &[];

pub fn render(frame: &mut Frame, area: Rect, chain: &mut ChainState) {
    let now = SystemTime::now();

    if area.height < MIN_HEIGHT_FOR_TABLE {
        render_dynamics(frame, area, chain, now, false);
        return;
    }

    let sections = Layout::vertical([Constraint::Percentage(55), Constraint::Min(6)]).split(area);
    let top = sections[0];
    let bottom = sections[1];

    let cols =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).split(top);

    let bands = chain
        .latest_snapshot
        .as_ref()
        .map(|s| fee_bands(&s.txs))
        .unwrap_or_default();
    fee_panel::render(
        frame,
        cols[0],
        "fee bands",
        &bands,
        &chain.meta.fee_rate_unit,
    );

    let show_sparklines = area.height >= MIN_HEIGHT_FOR_SPARKLINES;
    render_dynamics(frame, cols[1], chain, now, show_sparklines);

    let title = format!("{} mempool", chain.meta.display_name);
    let txs = chain
        .latest_snapshot
        .as_ref()
        .map(|s| s.txs.as_slice())
        .unwrap_or(EMPTY_TXS);
    tx_table::render(
        frame,
        bottom,
        &title,
        txs,
        &chain.meta.fee_rate_unit,
        now,
        &mut chain.tx_table,
    );
}

fn render_dynamics(
    frame: &mut Frame,
    area: Rect,
    chain: &ChainState,
    now: SystemTime,
    show_sparklines: bool,
) {
    let block = Block::bordered().title("pool dynamics");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let stats = chain.tracker.stats(now);
    let residency = stats
        .median_residency
        .map(humanize_secs_ago)
        .unwrap_or_else(|| "\u{2014}".to_string());

    let mut lines = vec![
        Line::from(format!("median residency: {residency}")),
        Line::from(format!(
            "departed (mined/dropped): {}",
            stats.departed_total
        )),
    ];
    if chain.is_cryptonote {
        if let Some(ds) = chain.latest_stats.as_ref().and_then(|s| s.double_spends) {
            lines.push(Line::from(format!("double spends seen: {ds}")));
        }
    }
    lines.push(Line::from(format!(
        "inflow: {:.1}/min (1m)  {:.1}/min (5m)  {:.1}/min (15m)",
        stats.inflow_1m, stats.inflow_5m, stats.inflow_15m
    )));

    if !show_sparklines {
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let text_h = lines.len() as u16;
    let split = Layout::vertical([Constraint::Length(text_h), Constraint::Min(3)]).split(inner);

    frame.render_widget(Paragraph::new(lines), split[0]);

    let rows = Layout::vertical([Constraint::Ratio(1, 3); 3]).split(split[1]);
    let style = Style::default().fg(Color::Blue);
    sparkline::render(frame, rows[0], "tx count", &chain.tx_count_series, style);
    sparkline::render(
        frame,
        rows[1],
        "pool bytes",
        &chain.pool_bytes_series,
        style,
    );
    sparkline::render(frame, rows[2], "inflow/min", &chain.inflow_series, style);
}
