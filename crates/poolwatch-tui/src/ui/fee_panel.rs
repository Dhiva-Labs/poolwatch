//! Fee-band panel: horizontal bars for the p10..p90 fee-per-byte bands.
//!
//! Uses a single-hue (blue) dim-to-bold intensity ramp across the bands.
//! Red/green are reserved for chain status elsewhere and must never appear
//! here.

use poolwatch_core::FeeBand;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::ui::fmt_fee_rate;

/// Single-hue intensity ramp: dimmest at the low end, boldest at the high
/// end, plain in between.
fn band_style(index: usize, len: usize) -> Style {
    let base = Style::default().fg(Color::Blue);
    if len <= 1 {
        return base.add_modifier(Modifier::BOLD);
    }
    if index == 0 {
        base.add_modifier(Modifier::DIM)
    } else if index + 1 == len {
        base.add_modifier(Modifier::BOLD)
    } else {
        base
    }
}

/// Space reserved for the "p90 " label and " 12.3 sat/vB (7 tx)" trailer
/// around the bar itself.
const LABEL_AND_VALUE_WIDTH: u16 = 24;
const MIN_BAR_WIDTH: u16 = 4;

pub fn render(frame: &mut Frame, area: Rect, title: &str, bands: &[FeeBand], fee_rate_unit: &str) {
    let block = Block::bordered().title(title.to_string());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if bands.is_empty() {
        frame.render_widget(Paragraph::new("no pool data"), inner);
        return;
    }

    let max = bands
        .iter()
        .map(|b| b.fee_per_byte)
        .fold(0.0_f64, f64::max)
        .max(f64::EPSILON);

    let bar_width = inner
        .width
        .saturating_sub(LABEL_AND_VALUE_WIDTH)
        .max(MIN_BAR_WIDTH);

    let lines: Vec<Line> = bands
        .iter()
        .enumerate()
        .map(|(i, band)| bar_line(band, i, bands.len(), max, bar_width, fee_rate_unit))
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

fn bar_line(
    band: &FeeBand,
    index: usize,
    total: usize,
    max: f64,
    bar_width: u16,
    fee_rate_unit: &str,
) -> Line<'static> {
    let style = band_style(index, total);
    let filled = ((band.fee_per_byte / max) * f64::from(bar_width)).round() as u16;
    let bar: String = "\u{2588}".repeat(filled.min(bar_width) as usize);
    let value = fmt_fee_rate(band.fee_per_byte, fee_rate_unit);
    Line::from(vec![
        Span::styled(format!("{:>4} ", band.label), style),
        Span::styled(bar, style),
        Span::raw(format!(" {value} ({} tx)", band.tx_count)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_style_dims_first_bolds_last_plain_between() {
        let dim = band_style(0, 5);
        let mid = band_style(2, 5);
        let bold = band_style(4, 5);
        assert!(dim.add_modifier.contains(Modifier::DIM));
        assert!(!mid.add_modifier.contains(Modifier::DIM));
        assert!(!mid.add_modifier.contains(Modifier::BOLD));
        assert!(bold.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn band_style_never_uses_red_or_green() {
        for i in 0..5 {
            let style = band_style(i, 5);
            assert_ne!(style.fg, Some(Color::Red));
            assert_ne!(style.fg, Some(Color::Green));
            assert_eq!(style.fg, Some(Color::Blue));
        }
    }

    #[test]
    fn band_style_single_band_is_bold() {
        let style = band_style(0, 1);
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }
}
