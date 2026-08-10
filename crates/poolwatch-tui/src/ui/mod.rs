//! Rendering: the top-level `draw` dispatcher, header/footer/help chrome,
//! and formatting helpers shared by every panel.
//!
//! Minimum supported terminal size is 80x24; below that (and below other
//! per-view thresholds) panels degrade gracefully -- sparklines hide first,
//! then the tx table -- rather than panicking on a too-small `Rect`.

pub mod chain_view;
pub mod dashboard;
pub mod fee_panel;
pub mod sparkline;
pub mod tx_table;

use std::time::{Duration, Instant};

use poolwatch_core::{InstantTxKind, PoolTx};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, ChainStatus, View};

/// Footer reserves one line for keybinding hints and one for the selected
/// chain's last-error line.
const FOOTER_HEIGHT: u16 = 2;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(FOOTER_HEIGHT),
    ])
    .split(area);

    render_header(frame, sections[0], app);

    match app.view {
        View::Dashboard => dashboard::render(frame, sections[1], app),
        View::Detail => {
            let idx = app.selected;
            if idx < app.chains.len() {
                chain_view::render(frame, sections[1], &mut app.chains[idx]);
            } else {
                frame.render_widget(Paragraph::new("no chains configured"), sections[1]);
            }
        }
    }

    render_footer(frame, sections[2], app);

    if app.show_help {
        render_help_overlay(frame, area);
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![Span::styled(
        "poolwatch",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    match app.view {
        View::Dashboard => spans.push(Span::raw("  dashboard")),
        View::Detail => {
            if let Some(chain) = app.selected_chain() {
                spans.push(Span::raw(format!(
                    "  {} ({})",
                    chain.meta.display_name, chain.meta.ticker
                )));
            }
        }
    }
    if app.paused.load(std::sync::atomic::Ordering::Relaxed) {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            " PAUSED ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    const HINTS: &str = "q quit  d dashboard  1-9/Tab chain  \u{2191}\u{2193}/jk move  \
                          f/s/t sort  / filter  p pause  ? help";

    let (status_line, color) = match app.selected_chain().map(|c| &c.status) {
        Some(ChainStatus::Unreachable { error, retry_at }) => {
            let remaining = retry_at.saturating_duration_since(Instant::now());
            (
                format!(
                    "last error: {error} \u{2014} retry in {}",
                    humanize_secs_ago(remaining)
                ),
                Color::Red,
            )
        }
        Some(ChainStatus::Degraded { reason }) => (format!("degraded: {reason}"), Color::Yellow),
        _ => (String::new(), Color::Reset),
    };

    let lines = vec![
        Line::from(Span::styled(
            HINTS,
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(status_line, Style::default().fg(color))),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_help_overlay(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(60, 70, area);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(Span::styled(
            "poolwatch keybindings",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("q          quit"),
        Line::from("d          dashboard view"),
        Line::from("1-9        jump to chain N's detail view"),
        Line::from("Tab        cycle to the next chain (detail view)"),
        Line::from("Enter      open detail view for the selected chain"),
        Line::from("\u{2191}\u{2193} / j k    move selection"),
        Line::from("f / s / t  sort tx table by fee / size / age"),
        Line::from("/          filter tx table by txid prefix"),
        Line::from("p          pause/resume polling"),
        Line::from("?          toggle this help"),
        Line::from("Esc        close prompt/help, or back to dashboard"),
    ];
    let block = Block::bordered().title("Help");
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}

pub fn status_color(status: &ChainStatus) -> Color {
    match status {
        ChainStatus::Ok => Color::Green,
        ChainStatus::Degraded { .. } => Color::Yellow,
        ChainStatus::Unreachable { .. } => Color::Red,
    }
}

/// Dashboard "INSTANT" column: counts Blink/Flash txs in the given slice.
/// Chains only ever produce one instant kind (config-enforced), so the
/// first non-zero count found is shown.
pub fn instant_summary(txs: &[PoolTx]) -> String {
    let blink = txs
        .iter()
        .filter(|t| t.instant == InstantTxKind::Blink)
        .count();
    let flash = txs
        .iter()
        .filter(|t| t.instant == InstantTxKind::Flash)
        .count();
    if blink > 0 {
        format!("\u{26a1} {blink} blink")
    } else if flash > 0 {
        format!("\u{26a1} {flash} flash")
    } else {
        "\u{2014}".to_string()
    }
}

/// Formats a byte count as a short human string: "512 B", "3.2 KB",
/// "1.4 MB", "2.0 GB". Whole bytes get no decimal; KB and above get one.
pub fn humanize_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let b = bytes as f64;
    if b < MB {
        format!("{:.1} KB", b / KB)
    } else if b < GB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.1} GB", b / GB)
    }
}

/// Compact duration string: "14s", "2m 14s", "1h 02m". Whole seconds only
/// (truncates any fractional part). Callers append "ago" / "left" / etc.
/// as appropriate for the context -- this function only formats magnitude.
pub fn humanize_secs_ago(d: Duration) -> String {
    let total = d.as_secs();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Formats a fee rate with adaptive precision: 0 decimals at/above 100,
/// 1 decimal at/above 10, else 2 decimals. Appends `unit` (e.g. "sat/vB").
pub fn fmt_fee_rate(rate: f64, unit: &str) -> String {
    let precision = if rate >= 100.0 {
        0
    } else if rate >= 10.0 {
        1
    } else {
        2
    };
    format!("{rate:.precision$} {unit}")
}

/// Shortens a txid to its first 16 characters, appending an ellipsis if it
/// was longer. Operates on chars (not bytes) so it never panics on
/// non-ASCII input, even though real txids are always hex/ASCII.
pub fn short_txid(txid: &str) -> String {
    if txid.chars().count() <= 16 {
        return txid.to_string();
    }
    let prefix: String = txid.chars().take(16).collect();
    format!("{prefix}\u{2026}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_bytes_under_1kb_has_no_decimal() {
        assert_eq!(humanize_bytes(0), "0 B");
        assert_eq!(humanize_bytes(512), "512 B");
        assert_eq!(humanize_bytes(1023), "1023 B");
    }

    #[test]
    fn humanize_bytes_kb_range() {
        assert_eq!(humanize_bytes(1024), "1.0 KB");
        assert_eq!(humanize_bytes(3277), "3.2 KB"); // 3200/1024 = 3.125 -> 3.2 rounds
    }

    #[test]
    fn humanize_bytes_mb_and_gb_range() {
        assert_eq!(humanize_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(humanize_bytes(1_468_006_400), "1.4 GB");
    }

    #[test]
    fn humanize_secs_ago_seconds_only() {
        assert_eq!(humanize_secs_ago(Duration::from_secs(0)), "0s");
        assert_eq!(humanize_secs_ago(Duration::from_secs(14)), "14s");
        assert_eq!(humanize_secs_ago(Duration::from_secs(59)), "59s");
    }

    #[test]
    fn humanize_secs_ago_minutes_and_seconds() {
        assert_eq!(humanize_secs_ago(Duration::from_secs(134)), "2m 14s");
        assert_eq!(humanize_secs_ago(Duration::from_secs(60)), "1m 00s");
    }

    #[test]
    fn humanize_secs_ago_hours_and_minutes() {
        assert_eq!(humanize_secs_ago(Duration::from_secs(3720)), "1h 02m");
        assert_eq!(humanize_secs_ago(Duration::from_secs(3600)), "1h 00m");
    }

    #[test]
    fn fmt_fee_rate_adaptive_precision() {
        assert_eq!(fmt_fee_rate(123.456, "sat/vB"), "123 sat/vB");
        assert_eq!(fmt_fee_rate(99.99, "sat/vB"), "100.0 sat/vB");
        assert_eq!(fmt_fee_rate(12.345, "au/B"), "12.3 au/B");
        assert_eq!(fmt_fee_rate(9.999, "au/B"), "10.00 au/B");
        assert_eq!(fmt_fee_rate(1.2345, "au/B"), "1.23 au/B");
        assert_eq!(fmt_fee_rate(0.0, "au/B"), "0.00 au/B");
    }

    #[test]
    fn short_txid_truncates_long_ids() {
        let id = "abcdef0123456789abcdef0123456789";
        assert_eq!(short_txid(id), "abcdef0123456789\u{2026}");
    }

    #[test]
    fn short_txid_leaves_short_ids_alone() {
        assert_eq!(short_txid("abc123"), "abc123");
        assert_eq!(short_txid("0123456789abcdef"), "0123456789abcdef"); // exactly 16
    }

    #[test]
    fn instant_summary_counts_blink_and_flash() {
        use poolwatch_core::PoolTx;
        use std::time::SystemTime;

        fn tx(instant: InstantTxKind) -> PoolTx {
            PoolTx {
                txid: "a".to_string(),
                size_bytes: 100,
                fee_atomic: 100,
                fee_per_byte: 1.0,
                received_by_node: None,
                first_seen_local: SystemTime::UNIX_EPOCH,
                instant,
                in_out_counts: None,
                double_spend_seen: false,
            }
        }

        assert_eq!(instant_summary(&[]), "\u{2014}");
        assert_eq!(
            instant_summary(&[tx(InstantTxKind::None), tx(InstantTxKind::None)]),
            "\u{2014}"
        );
        assert_eq!(
            instant_summary(&[
                tx(InstantTxKind::Blink),
                tx(InstantTxKind::Blink),
                tx(InstantTxKind::None)
            ]),
            "\u{26a1} 2 blink"
        );
        assert_eq!(
            instant_summary(&[tx(InstantTxKind::Flash)]),
            "\u{26a1} 1 flash"
        );
    }
}
