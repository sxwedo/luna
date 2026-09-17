use crate::domain::quota::AccountQuota;
use crate::ui::progress::{format_percent, render_smooth_bar};
use colored::Colorize;
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, ContentArrangement, Row, Table};

fn table_color(percentage: f64) -> Color {
    if percentage >= 60.0 {
        Color::Green
    } else if percentage >= 25.0 {
        Color::Yellow
    } else {
        Color::Red
    }
}

/// Prints a consolidated table of all account quotas.
pub fn print_quota_overview(quotas: &[AccountQuota]) {
    if quotas.is_empty() {
        println!("{}", "No accounts found to display.".dimmed());
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Disabled)
        .set_header(vec![
            Cell::new("Account").fg(Color::Cyan),
            Cell::new("Provider").fg(Color::Magenta),
            Cell::new("Plan").fg(Color::Blue),
            Cell::new("Window").fg(Color::White),
            Cell::new("Left").fg(Color::Yellow),
            Cell::new("Bar").fg(Color::Green),
            Cell::new("Reset").fg(Color::White),
        ]);

    for quota in quotas {
        if let Some(err) = &quota.error {
            table.add_row(Row::from(vec![
                Cell::new(&quota.label),
                Cell::new(quota.provider.as_str()),
                Cell::new("Error").fg(Color::Red),
                Cell::new(err).fg(Color::Red),
                Cell::new("-"),
                Cell::new("-"),
                Cell::new("-"),
            ]));
            continue;
        }

        let plan_str = quota.plan.as_deref().unwrap_or("Standard / Free");
        let provider_str = quota.provider.as_str();

        if quota.windows.is_empty() {
            table.add_row(Row::from(vec![
                Cell::new(&quota.label),
                Cell::new(provider_str),
                Cell::new(plan_str),
                Cell::new("No metrics"),
                Cell::new("-"),
                Cell::new("-"),
                Cell::new("-"),
            ]));
            continue;
        }

        for (idx, window) in quota.windows.iter().enumerate() {
            let color = table_color(window.remaining_percent);
            table.add_row(Row::from(vec![
                Cell::new(if idx == 0 { quota.label.as_str() } else { "" }),
                Cell::new(if idx == 0 { provider_str } else { "" }),
                Cell::new(if idx == 0 { plan_str } else { "" }),
                Cell::new(&window.name),
                Cell::new(format!("{:>5.1}%", window.remaining_percent)).fg(color),
                Cell::new(render_smooth_bar(window.remaining_percent, 12)),
                Cell::new(window.format_reset_time()),
            ]));
        }
    }

    println!("\n{}", "=== LLM Account Quota Monitor ===".bold().cyan());
    println!("{}", table);

    let bottlenecks: Vec<_> = quotas
        .iter()
        .filter_map(|quota| quota.bottleneck().map(|bn| (&quota.label, bn)))
        .collect();

    if !bottlenecks.is_empty() {
        println!(
            "\n{}",
            "Lowest remaining window (bottleneck):".bold().yellow()
        );
        for (label, bn) in bottlenecks {
            println!(
                "  - {}: {}  {}  resets {}",
                label.bold(),
                bn.name,
                format_percent(bn.remaining_percent),
                bn.format_reset_time().cyan()
            );
        }
    }
    println!();
}
