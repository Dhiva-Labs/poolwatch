//! Sparkline rendering wrapper around `poolwatch_core::stats::TimeSeries`.

use poolwatch_core::stats::TimeSeries;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Sparkline};
use ratatui::Frame;

/// Converts a `TimeSeries` into sparkline bars: rounded to the nearest
/// integer, floored at 0 (values shouldn't go negative for tx count / pool
/// bytes / inflow, but a defensive floor avoids an `as u64` cast wrapping if
/// they ever did).
pub fn series_to_bars(series: &TimeSeries) -> Vec<u64> {
    series
        .iter()
        .map(|(_, v)| if *v <= 0.0 { 0 } else { v.round() as u64 })
        .collect()
}

/// Renders one titled, bordered sparkline into `area`.
pub fn render(frame: &mut Frame, area: Rect, title: &str, series: &TimeSeries, style: Style) {
    let bars = series_to_bars(series);
    let block = Block::bordered().title(title.to_string());
    let sparkline = Sparkline::default().block(block).data(bars).style(style);
    frame.render_widget(sparkline, area);
}

/// Renders a sparkline without a surrounding block, for tight strips (e.g.
/// the dashboard's per-chain rows).
pub fn render_bare(frame: &mut Frame, area: Rect, series: &TimeSeries, style: Style) {
    let bars = series_to_bars(series);
    let sparkline = Sparkline::default().data(bars).style(style);
    frame.render_widget(sparkline, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    // `TimeSeries::push` is `todo!()` in the frozen core crate today (see
    // the task contract), so this only exercises the empty case -- still
    // useful to prove `series_to_bars` doesn't panic on an empty series,
    // which is the state every chain starts in before its first stats tick.
    #[test]
    fn series_to_bars_is_empty_for_a_fresh_series() {
        let series = TimeSeries::new(4);
        assert!(series_to_bars(&series).is_empty());
    }
}
