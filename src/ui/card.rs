use crate::domain::quota::{AccountDiff, AccountQuota, QuotaPeriod};
use crate::ui::colors::{self, palette, rgb, ORBIT_TITLE_REST};
use crate::ui::progress::{format_percent, render_smooth_bar};
use chrono::Local;
use colored::Colorize;
use std::collections::HashMap;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const SINGLE_CARD_WIDTH: usize = 82;

/// Strips ANSI escape sequences to compute true visible terminal width.
fn visible_width(s: &str) -> usize {
    let mut clean = String::with_capacity(s.len());
    let mut in_escape = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if c == 'm' {
                in_escape = false;
            }
        } else {
            clean.push(c);
        }
    }
    UnicodeWidthStr::width(clean.as_str())
}

/// Renders all account quotas as modern, airy, anti-slop cards.
pub fn render_quota_cards(quotas: &[AccountQuota]) {
    render_quota_cards_animated(quotas, None, 0);
}

/// Renders quota cards with live change detection, auto multi-column responsive layout.
pub fn render_quota_cards_animated(
    quotas: &[AccountQuota],
    diffs: Option<&HashMap<String, AccountDiff>>,
    frame_tick: usize,
) {
    if quotas.is_empty() {
        println!(
            "{}",
            rgb("  No accounts configured to display.", palette::MUTED)
        );
        return;
    }

    let term_width = crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(80);
    let use_two_cols = term_width >= 155;

    let active_count = quotas.iter().filter(|q| q.error.is_none()).count();
    let current_time = Local::now().format("%H:%M:%S").to_string();

    let divider_width = if use_two_cols {
        (SINGLE_CARD_WIDTH * 2) + 4
    } else {
        SINGLE_CARD_WIDTH + 2
    };

    // Header
    println!();
    print!("  {}", rgb("LUNA", palette::WHITE).bold());
    print!(" {}", rgb("·", palette::MUTED));
    print!(" {}", rgb("remaining light", palette::MUTED));

    let status_info = format!("● {} active   {}", active_count, current_time);
    let status_colored = rgb(&status_info, palette::EMERALD);
    let pad_header = divider_width.saturating_sub(25 + status_info.chars().count());
    println!("{:>pad$}", status_colored, pad = pad_header);

    println!("  {}", rgb(&"─".repeat(divider_width), palette::BORDER));
    println!();

    // Render each account into a block of lines
    let card_blocks: Vec<Vec<String>> = quotas
        .iter()
        .map(|q| {
            let diff = diffs.and_then(|d| d.get(&q.account_id));
            build_card_lines(q, diff, frame_tick, SINGLE_CARD_WIDTH)
        })
        .collect();

    if use_two_cols {
        // Pair cards side by side
        for chunk in card_blocks.chunks(2) {
            if chunk.len() == 2 {
                let left = &chunk[0];
                let right = &chunk[1];
                let max_lines = left.len().max(right.len());
                for line_idx in 0..max_lines {
                    let left_line = left
                        .get(line_idx)
                        .cloned()
                        .unwrap_or_else(|| " ".repeat(SINGLE_CARD_WIDTH + 2));
                    let right_line = right.get(line_idx).cloned().unwrap_or_default();
                    println!("  {}  {}", left_line, right_line);
                }
            } else {
                for line in &chunk[0] {
                    println!("  {}", line);
                }
            }
            println!();
        }
    } else {
        for block in card_blocks {
            for line in block {
                println!("  {}", line);
            }
            println!();
        }
    }

    render_fleet_summary_line(quotas);
}

/// Builds an individual card as a vector of lines with exact visible width.
fn build_card_lines(
    quota: &AccountQuota,
    diff: Option<&AccountDiff>,
    frame_tick: usize,
    card_width: usize,
) -> Vec<String> {
    let provider_name = quota.provider.display_name();
    let plan_text = quota.plan.as_deref().unwrap_or("Standard");
    let is_consuming = diff.map(|d| d.is_consuming).unwrap_or(false);

    let mut inner: Vec<String> = Vec::new();
    inner.push(String::new()); // top padding

    if let Some(err) = &quota.error {
        inner.push(format!(
            "   {} {}",
            rgb("✖ Error:", palette::ROSE).bold(),
            rgb(err, palette::MUTED)
        ));
    } else if quota.windows.is_empty() {
        inner.push(format!(
            "   {}",
            rgb("No active quota metrics reported.", palette::MUTED)
        ));
    } else {
        for window in &quota.windows {
            inner.push(render_window_row(window, diff));
        }
    }
    inner.push(String::new()); // bottom padding

    let title = if is_consuming {
        format!(
            " {} · {} · {}  ⚡ CONSUMING ",
            quota.label, provider_name, plan_text
        )
    } else {
        format!(" {} · {} · {} ", quota.label, provider_name, plan_text)
    };

    if is_consuming {
        enclose_orbit_card(&title, &inner, card_width, frame_tick)
    } else {
        enclose_static_card(&title, &inner, card_width)
    }
}

fn render_window_row(
    window: &crate::domain::quota::QuotaWindow,
    diff: Option<&AccountDiff>,
) -> String {
    let icon = match window.period {
        QuotaPeriod::Session5H => "⚡",
        QuotaPeriod::Weekly7D => "📅",
        _ => "◇",
    };
    // Normalize icon to EXACTLY 3 visible terminal columns:
    // ⚡/📅 are width-2 emojis, ◇ is width-1 → pad with one extra space.
    let icon_normalized = match icon {
        "◇" => "◇ ",
        other => other,
    };
    let name_width = UnicodeWidthStr::width(window.name.as_str());
    let name_padded = format!(
        "{}{}",
        window.name,
        " ".repeat(20usize.saturating_sub(name_width))
    );
    let bar = render_smooth_bar(window.remaining_percent, 18);
    let pct = format_percent(window.remaining_percent);
    let delta_text = match diff.and_then(|d| d.delta_for_window(&window.name)) {
        Some(delta) if delta <= -0.05 => {
            let formatted = format!("↓ {:>4.1}%", delta.abs());
            format!(" {} ", rgb(&formatted, palette::AMBER).bold())
        }
        Some(delta) if delta >= 5.0 => {
            format!(" {} ", rgb("↑ RESET", palette::EMERALD).bold())
        }
        _ => "         ".to_string(),
    };
    let reset_str = format!("resets in {}", window.format_reset_time());
    format!(
        "   {} {} {}  {} {}{}",
        icon_normalized,
        rgb(&name_padded, palette::WHITE),
        bar,
        pct,
        delta_text,
        rgb(&reset_str, palette::MUTED)
    )
}

fn enclose_static_card(title: &str, inner: &[String], card_width: usize) -> Vec<String> {
    let header_visible_len = UnicodeWidthStr::width(title);
    let fill_len = card_width.saturating_sub(header_visible_len + 3);
    let mut lines = Vec::with_capacity(inner.len() + 2);
    lines.push(format!(
        "{}{}{}{}",
        rgb("╭─", palette::BORDER),
        rgb(title, palette::WHITE).bold(),
        rgb(&"─".repeat(fill_len), palette::BORDER),
        rgb("╮", palette::BORDER)
    ));
    for content in inner {
        lines.push(format_enclosed_line(
            content,
            card_width,
            "│",
            palette::BORDER,
        ));
    }
    lines.push(format!(
        "{}{}{}",
        rgb("╰", palette::BORDER),
        rgb(&"─".repeat(card_width.saturating_sub(2)), palette::BORDER),
        rgb("╯", palette::BORDER)
    ));
    lines
}

/// One travelling highlight: title letters → remaining top edge → around the
/// chrome → back onto the title. Resting border stays slate; no rainbow flash.
fn enclose_orbit_card(
    title: &str,
    inner: &[String],
    card_width: usize,
    frame: usize,
) -> Vec<String> {
    let height = inner.len() + 2;
    let mut lines = Vec::with_capacity(height);
    lines.push(paint_orbit_top(title, card_width, height, frame));
    for (i, content) in inner.iter().enumerate() {
        let y = i + 1;
        let vis = visible_width(content);
        let pad = card_width.saturating_sub(2).saturating_sub(vis);
        lines.push(format!(
            "{}{}{}{}",
            colors::orbit_cell('│', 0, y, card_width, height, frame, palette::BORDER),
            content,
            " ".repeat(pad),
            colors::orbit_cell(
                '│',
                card_width - 1,
                y,
                card_width,
                height,
                frame,
                palette::BORDER
            ),
        ));
    }
    lines.push(paint_orbit_bottom(card_width, height, frame));
    lines
}

fn paint_orbit_top(title: &str, width: usize, height: usize, frame: usize) -> String {
    let mut cols: Vec<Option<char>> = vec![Some('─'); width];
    if width == 0 {
        return String::new();
    }
    cols[0] = Some('╭');
    cols[width - 1] = Some('╮');

    let mut cursor = colors::ORBIT_TITLE_ORIGIN;
    for ch in title.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
        if cursor + cw >= width {
            break;
        }
        cols[cursor] = Some(ch);
        for k in 1..cw {
            cols[cursor + k] = None;
        }
        cursor += cw;
    }
    let title_end = cursor;

    let mut out = String::with_capacity(width * 16);
    for x in 0..width {
        let Some(ch) = cols[x] else {
            continue;
        };
        let base = if x >= colors::ORBIT_TITLE_ORIGIN && x < title_end {
            ORBIT_TITLE_REST
        } else {
            palette::BORDER
        };
        out.push_str(&colors::orbit_cell(ch, x, 0, width, height, frame, base));
    }
    out
}

fn paint_orbit_bottom(width: usize, height: usize, frame: usize) -> String {
    let mut out = String::with_capacity(width * 16);
    for x in 0..width {
        let ch = if x == 0 {
            '╰'
        } else if x + 1 == width {
            '╯'
        } else {
            '─'
        };
        out.push_str(&colors::orbit_cell(
            ch,
            x,
            height - 1,
            width,
            height,
            frame,
            palette::BORDER,
        ));
    }
    out
}

/// Closes a line between left and right borders with exact visual width.
fn format_enclosed_line(
    content: &str,
    total_width: usize,
    side_char: &str,
    border_color: (u8, u8, u8),
) -> String {
    let vis = visible_width(content);
    let inner_target = total_width.saturating_sub(2);
    let pad = inner_target.saturating_sub(vis);
    format!(
        "{}{}{}{}",
        rgb(side_char, border_color),
        content,
        " ".repeat(pad),
        rgb(side_char, border_color)
    )
}

fn render_fleet_summary_line(quotas: &[AccountQuota]) {
    let insights = crate::domain::quota::compute_fleet_insights(quotas);
    let mut parts = Vec::new();

    if let Some((ready_label, pct)) = insights.best_ready {
        parts.push(format!(
            "{} {}",
            rgb("✦ Ready:", palette::EMERALD).bold(),
            rgb(&format!("{} ({:.0}%)", ready_label, pct), palette::WHITE)
        ));
    }

    if let Some((reset_label, win_name, time_str)) = insights.next_reset {
        parts.push(format!(
            "{} {}",
            rgb("⏱ Next reset:", palette::AMBER).bold(),
            rgb(
                &format!("{} in {} ({})", win_name, time_str, reset_label),
                palette::WHITE
            )
        ));
    }

    if !parts.is_empty() {
        println!(
            "  {}",
            parts.join(&format!("  {}  ", rgb("·", palette::MUTED)))
        );
        println!();
    }
}
