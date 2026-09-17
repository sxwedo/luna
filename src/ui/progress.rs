use crate::ui::colors::{palette, quota_color, rgb};

const FRACTIONS: [&str; 8] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// Renders a high-precision Unicode sub-character progress bar with custom width and RGB color.
pub fn render_smooth_bar(percentage: f64, width: usize) -> String {
    let pct = percentage.clamp(0.0, 100.0);
    let color = quota_color(pct);
    let total_subblocks = width * 8;
    let filled_subblocks = ((pct / 100.0) * (total_subblocks as f64)).round() as usize;

    let full_blocks = filled_subblocks / 8;
    let remainder = filled_subblocks % 8;

    let mut bar = String::new();

    // Full filled blocks
    if full_blocks > 0 {
        bar.push_str(&"█".repeat(full_blocks.min(width)));
    }

    // Fractional block
    if full_blocks < width && remainder > 0 {
        bar.push_str(FRACTIONS[remainder]);
    }

    let current_len = full_blocks + if remainder > 0 { 1 } else { 0 };
    let empty_len = width.saturating_sub(current_len);

    let colored_filled = rgb(&bar, color);
    let colored_track = rgb(&"░".repeat(empty_len), palette::TRACK);

    format!("{}{}", colored_filled, colored_track)
}

/// Formats a percentage string with its semantic truecolor color.
pub fn format_percent(percentage: f64) -> String {
    let text = format!("{:>5.1}%", percentage.clamp(0.0, 100.0));
    let color = quota_color(percentage);
    rgb(&text, color)
}
