use crate::domain::quota::{AccountDiff, AccountQuota, QuotaPeriod};
use crate::ui::colors::{self, palette, ORBIT_TITLE_REST};
use chrono::Local;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame, Terminal,
};
use std::collections::HashMap;
use std::io::stdout;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthChar;

fn to_ratatui_color((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

/// Represents the active TUI state in watch mode.
pub struct TuiApp {
    pub quotas: Vec<AccountQuota>,
    pub diffs: HashMap<String, AccountDiff>,
    pub frame_tick: usize,
    pub countdown_seconds: usize,
    pub is_refreshing: bool,
}

impl TuiApp {
    pub fn new(initial_quotas: Vec<AccountQuota>) -> Self {
        Self {
            quotas: initial_quotas,
            diffs: HashMap::new(),
            frame_tick: 0,
            countdown_seconds: 60,
            is_refreshing: false,
        }
    }
}

/// Run full interactive responsive TUI watch dashboard.
pub async fn run_watch_tui(
    vault: crate::storage::vault::AccountVault,
    filter_provider: Option<String>,
    filter_account: Option<String>,
    initial_quotas: Vec<AccountQuota>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = TuiApp::new(initial_quotas);
    let mut previous_quotas: Option<Vec<AccountQuota>> = None;

    let (tx, mut rx) = mpsc::channel::<Vec<AccountQuota>>(2);
    let mut tick_timer = tokio::time::interval(Duration::from_millis(100));
    let mut second_counter = 0usize;

    loop {
        terminal.draw(|f| ui(f, &app))?;

        tokio::select! {
            Some(fresh) = rx.recv() => {
                let diff_map = previous_quotas.as_ref().map(|p| crate::domain::quota::compute_quota_diffs(p, &fresh));
                if let Some(map) = diff_map {
                    app.diffs = map;
                }
                previous_quotas = Some(fresh.clone());
                app.quotas = fresh;
                app.countdown_seconds = 60;
                app.is_refreshing = false;
            }

            _ = tick_timer.tick() => {
                app.frame_tick = app.frame_tick.wrapping_add(1);
                second_counter += 1;

                if second_counter >= 10 {
                    second_counter = 0;
                    if app.countdown_seconds > 0 {
                        app.countdown_seconds -= 1;
                    }

                    if app.countdown_seconds == 0 && !app.is_refreshing {
                        app.is_refreshing = true;
                        let tx_clone = tx.clone();
                        let vault_clone = vault.clone();
                        let fp = filter_provider.clone();
                        let fa = filter_account.clone();

                        tokio::spawn(async move {
                            if let Ok(quotas) = fetch_quotas_job(&vault_clone, fp.as_deref(), fa.as_deref()).await {
                                let _ = tx_clone.send(quotas).await;
                            }
                        });
                    }
                }
            }
        }

        if event::poll(Duration::from_millis(10))? {
            if let Event::Key(key) = event::read()? {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('r') if !app.is_refreshing => {
                        app.is_refreshing = true;
                        let tx_clone = tx.clone();
                        let vault_clone = vault.clone();
                        let fp = filter_provider.clone();
                        let fa = filter_account.clone();

                        tokio::spawn(async move {
                            if let Ok(quotas) =
                                fetch_quotas_job(&vault_clone, fp.as_deref(), fa.as_deref()).await
                            {
                                let _ = tx_clone.send(quotas).await;
                            }
                        });
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

async fn fetch_quotas_job(
    vault: &crate::storage::vault::AccountVault,
    filter_provider: Option<&str>,
    filter_account: Option<&str>,
) -> anyhow::Result<Vec<AccountQuota>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()?;
    let mut accounts = vault.list()?;

    if let Some(prov_str) = filter_provider {
        if let Ok(p_id) = prov_str.parse::<crate::domain::account::ProviderId>() {
            accounts.retain(|a| a.provider == p_id);
        }
    }

    if let Some(acc_str) = filter_account {
        accounts.retain(|a| a.id == acc_str || a.label.contains(acc_str));
    }

    let client = Arc::new(client);
    let mut join_set = tokio::task::JoinSet::new();

    for mut account in accounts {
        let client_clone = Arc::clone(&client);
        join_set.spawn(async move {
            let provider_impl = crate::providers::ProviderRegistry::get(account.provider);
            if account.credential.is_expired(300) {
                let _ = provider_impl
                    .refresh_token(&mut account.credential, &client_clone)
                    .await;
            }
            provider_impl
                .fetch_quota(&account, &client_clone)
                .await
                .unwrap_or_else(|err| {
                    AccountQuota::failed(
                        &account.id,
                        account.provider,
                        &account.label,
                        err.to_string(),
                    )
                })
        });
    }

    let mut results = Vec::new();
    while let Some(res) = join_set.join_next().await {
        if let Ok(q) = res {
            results.push(q);
        }
    }
    results.sort_by(|a, b| {
        a.provider
            .as_str()
            .cmp(b.provider.as_str())
            .then_with(|| a.label.cmp(&b.label))
    });
    Ok(results)
}

/// Top-level TUI render function.
fn ui(f: &mut Frame, app: &TuiApp) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(if app.quotas.is_empty() { 0 } else { 7 }),
            Constraint::Length(1),
        ])
        .split(area);

    render_tui_header(f, chunks[0], app);
    render_responsive_cards_grid(f, chunks[1], app);
    render_tui_bottlenecks(f, chunks[2], app);
    render_tui_statusbar(f, chunks[3], app);
}

fn render_tui_header(f: &mut Frame, area: Rect, app: &TuiApp) {
    let active_count = app.quotas.iter().filter(|q| q.error.is_none()).count();
    let current_time = Local::now().format("%H:%M:%S").to_string();

    let header_line = Line::from(vec![
        Span::styled(
            " LUNA",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", Style::default().fg(to_ratatui_color(palette::MUTED))),
        Span::styled(
            "remaining light",
            Style::default().fg(to_ratatui_color(palette::MUTED)),
        ),
        Span::styled(
            format!(
                "               ● {} active   {}",
                active_count, current_time
            ),
            Style::default().fg(to_ratatui_color(palette::EMERALD)),
        ),
    ]);

    let divider = Line::from(vec![Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(to_ratatui_color(palette::BORDER)),
    )]);

    let paragraph = Paragraph::new(vec![header_line, divider]);
    f.render_widget(paragraph, area);
}

fn render_responsive_cards_grid(f: &mut Frame, area: Rect, app: &TuiApp) {
    if app.quotas.is_empty() {
        let empty = Paragraph::new("No accounts configured.")
            .style(Style::default().fg(to_ratatui_color(palette::MUTED)))
            .alignment(Alignment::Center);
        f.render_widget(empty, area);
        return;
    }

    let num_columns = if area.width >= 140 { 2 } else { 1 };

    let col_constraints = if num_columns == 2 {
        vec![Constraint::Percentage(50), Constraint::Percentage(50)]
    } else {
        vec![Constraint::Percentage(100)]
    };

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(col_constraints)
        .split(area);

    let card_height = 8u16;

    for (idx, quota) in app.quotas.iter().enumerate() {
        let col_idx = idx % num_columns;
        let row_idx = idx / num_columns;

        let col_rect = columns[col_idx];
        let y_pos = col_rect.y + (row_idx as u16 * card_height);

        if y_pos + card_height > col_rect.y + col_rect.height {
            continue;
        }

        let card_rect = Rect {
            x: col_rect.x,
            y: y_pos,
            width: col_rect.width.saturating_sub(1),
            height: card_height,
        };

        let diff = app.diffs.get(&quota.account_id);
        render_tui_card(f, card_rect, quota, diff, app.frame_tick);
    }
}

fn render_tui_card(
    f: &mut Frame,
    area: Rect,
    quota: &AccountQuota,
    diff: Option<&AccountDiff>,
    frame_tick: usize,
) {
    let is_consuming = diff.map(|d| d.is_consuming).unwrap_or(false);

    let title_badge = if is_consuming {
        format!(
            " {} · {} · {}  ⚡ CONSUMING ",
            quota.label,
            quota.provider.display_name(),
            quota.plan.as_deref().unwrap_or("Standard")
        )
    } else {
        format!(
            " {} · {} · {} ",
            quota.label,
            quota.provider.display_name(),
            quota.plan.as_deref().unwrap_or("Standard")
        )
    };

    let inner_area = if is_consuming {
        render_orbit_border(f, area, &title_badge, frame_tick);
        Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        }
    } else {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(to_ratatui_color(palette::BORDER)))
            .title(Line::from(Span::styled(
                title_badge,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
        let inner = block.inner(area);
        f.render_widget(block, area);
        inner
    };

    if let Some(err) = &quota.error {
        let err_line = Line::from(vec![
            Span::styled(
                "✖ Error: ",
                Style::default()
                    .fg(to_ratatui_color(palette::ROSE))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(err, Style::default().fg(to_ratatui_color(palette::MUTED))),
        ]);
        let p = Paragraph::new(vec![Line::raw(""), err_line]);
        f.render_widget(p, inner_area);
        return;
    }

    if quota.windows.is_empty() {
        let p = Paragraph::new(vec![
            Line::raw(""),
            Line::from(Span::styled(
                "No active quota metrics reported.",
                Style::default().fg(to_ratatui_color(palette::MUTED)),
            )),
        ]);
        f.render_widget(p, inner_area);
        return;
    }

    // Render each quota window into dedicated slots:
    // Slot 0: [  ⚡ Claude/GPT Weekly ] (Length 25) -> Never truncated!
    // Slot 1: [██████████████░░░░░░░░]   (Min 16, expands to fill available width)
    // Slot 2: [ 100.0%]                 (Length 8)
    // Slot 3: [ ↓  0.1% ]               (Length 9)
    // Slot 4: [  resets in 4h 7m]       (Length 18, right-aligned)
    for (row_idx, window) in quota.windows.iter().enumerate() {
        if row_idx as u16 >= inner_area.height {
            break;
        }

        let row_rect = Rect {
            x: inner_area.x,
            y: inner_area.y + row_idx as u16,
            width: inner_area.width,
            height: 1,
        };

        let slots = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(25), // Expanded Name Slot: avoids "Claude/GPT Wee" truncation
                Constraint::Min(16),    // Flexible Bar Slot: expands to fill remaining space!
                Constraint::Length(8),  // Percentage Slot
                Constraint::Length(9),  // Delta Slot
                Constraint::Length(18), // Reset countdown Slot
            ])
            .split(row_rect);

        let icon = match window.period {
            QuotaPeriod::Session5H => "⚡",
            QuotaPeriod::Weekly7D => "📅",
            _ => "◇",
        };

        let pct_color = if window.remaining_percent >= 60.0 {
            to_ratatui_color(palette::EMERALD)
        } else if window.remaining_percent >= 25.0 {
            to_ratatui_color(palette::AMBER)
        } else {
            to_ratatui_color(palette::ROSE)
        };

        // Slot 0: Icon + Name (Guaranteed no truncation)
        let name_p = Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::raw(icon),
            Span::raw(" "),
            Span::styled(&window.name, Style::default().fg(Color::White)),
        ]));
        f.render_widget(name_p, slots[0]);

        // Slot 1: Progress Bar (Dynamically adapts to slot width!)
        let bar_width = (slots[1].width as usize).saturating_sub(1).clamp(12, 40);
        let pct = window.remaining_percent.clamp(0.0, 100.0);
        let total_subblocks = bar_width * 8;
        let filled_subblocks = ((pct / 100.0) * (total_subblocks as f64)).round() as usize;
        let full_blocks = filled_subblocks / 8;
        let remainder = filled_subblocks % 8;
        let fractions = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

        let mut bar_filled = String::new();
        if full_blocks > 0 {
            bar_filled.push_str(&"█".repeat(full_blocks.min(bar_width)));
        }
        if full_blocks < bar_width && remainder > 0 {
            bar_filled.push_str(fractions[remainder]);
        }
        let cur_len = full_blocks + if remainder > 0 { 1 } else { 0 };
        let empty_len = bar_width.saturating_sub(cur_len);
        let bar_empty = "░".repeat(empty_len);

        let bar_p = Paragraph::new(Line::from(vec![
            Span::styled(bar_filled, Style::default().fg(pct_color)),
            Span::styled(
                bar_empty,
                Style::default().fg(to_ratatui_color(palette::TRACK)),
            ),
        ]));
        f.render_widget(bar_p, slots[1]);

        // Slot 2: Percentage (Right-aligned)
        let pct_p = Paragraph::new(Line::from(Span::styled(
            format!("{:>6.1}%", window.remaining_percent),
            Style::default().fg(pct_color).add_modifier(Modifier::BOLD),
        )));
        f.render_widget(pct_p, slots[2]);

        // Slot 3: Delta indicator
        if let Some(delta) = diff.and_then(|d| d.delta_for_window(&window.name)) {
            if delta <= -0.05 {
                let delta_p = Paragraph::new(Line::from(Span::styled(
                    format!("↓ {:>4.1}%", delta.abs()),
                    Style::default()
                        .fg(to_ratatui_color(palette::AMBER))
                        .add_modifier(Modifier::BOLD),
                )));
                f.render_widget(delta_p, slots[3]);
            } else if delta >= 5.0 {
                let delta_p = Paragraph::new(Line::from(Span::styled(
                    "↑ RESET",
                    Style::default()
                        .fg(to_ratatui_color(palette::EMERALD))
                        .add_modifier(Modifier::BOLD),
                )));
                f.render_widget(delta_p, slots[3]);
            }
        }

        // Slot 4: Reset countdown
        let reset_p = Paragraph::new(Line::from(Span::styled(
            format!("resets in {}", window.format_reset_time()),
            Style::default().fg(to_ratatui_color(palette::MUTED)),
        )));
        f.render_widget(reset_p, slots[4]);
    }
}

/// Draw rounded chrome with a single travelling highlight: title → remaining
/// top edge → right → bottom → left → back onto the letters.
fn render_orbit_border(f: &mut Frame, area: Rect, title: &str, frame: usize) {
    let w = area.width as usize;
    let h = area.height as usize;
    if w < 4 || h < 2 {
        return;
    }

    let mut title_end = colors::ORBIT_TITLE_ORIGIN;
    for ch in title.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
        if title_end + cw >= w {
            break;
        }
        title_end += cw;
    }

    let buf = f.buffer_mut();
    let path_len = colors::perimeter_len(w, h);
    let origin = colors::ORBIT_TITLE_ORIGIN;

    let paint = |buf: &mut ratatui::buffer::Buffer,
                 x: usize,
                 y: usize,
                 symbol: &str,
                 base: (u8, u8, u8)| {
        let idx = colors::perimeter_index(x, y, w, h);
        let t = colors::orbit_intensity(idx, path_len, frame, origin);
        let (r, g, b) = colors::orbit_color(base, t);
        let mut style = Style::default().fg(Color::Rgb(r, g, b));
        if t > 0.4 {
            style = style.add_modifier(Modifier::BOLD);
        }
        buf[(area.x + x as u16, area.y + y as u16)]
            .set_symbol(symbol)
            .set_style(style);
    };

    // Top edge: ╭─ … ╮, skipping columns the title will occupy.
    for x in 0..w {
        if x >= colors::ORBIT_TITLE_ORIGIN && x < title_end {
            continue;
        }
        let symbol = if x == 0 {
            "╭"
        } else if x + 1 == w {
            "╮"
        } else {
            "─"
        };
        paint(buf, x, 0, symbol, palette::BORDER);
    }

    // Title glyphs sit on the top edge so the beam continues off the last letter
    // onto the remaining ─ and around the card.
    let mut col = colors::ORBIT_TITLE_ORIGIN;
    for ch in title.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
        if col + cw >= w {
            break;
        }
        paint(buf, col, 0, &ch.to_string(), ORBIT_TITLE_REST);
        col += cw;
    }

    // Right / left sides (corners already drawn).
    for y in 1..h.saturating_sub(1) {
        paint(buf, w - 1, y, "│", palette::BORDER);
        paint(buf, 0, y, "│", palette::BORDER);
    }

    // Bottom edge: ╰─ … ╯
    for x in 0..w {
        let symbol = if x == 0 {
            "╰"
        } else if x + 1 == w {
            "╯"
        } else {
            "─"
        };
        paint(buf, x, h - 1, symbol, palette::BORDER);
    }
}

fn render_tui_bottlenecks(f: &mut Frame, area: Rect, app: &TuiApp) {
    if app.quotas.is_empty() {
        return;
    }

    let mut bottlenecks: Vec<_> = app
        .quotas
        .iter()
        .filter_map(|q| q.bottleneck().map(|b| (&q.label, b)))
        .collect();

    if bottlenecks.is_empty() {
        return;
    }

    bottlenecks.sort_by(|a, b| a.1.remaining_percent.total_cmp(&b.1.remaining_percent));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(to_ratatui_color(palette::BORDER)))
        .title(Span::styled(
            " ⚠ BOTTLENECK LIMITS (Shortest remaining quotas first) ",
            Style::default()
                .fg(to_ratatui_color(palette::AMBER))
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    f.render_widget(block, area);

    for (idx, (label, bn)) in bottlenecks.iter().take(4).enumerate() {
        if idx as u16 >= inner.height {
            break;
        }

        let row_rect = Rect {
            x: inner.x,
            y: inner.y + idx as u16,
            width: inner.width,
            height: 1,
        };

        let row_slots = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(30), // Label
                Constraint::Length(22), // Window name (expanded to prevent truncation)
                Constraint::Length(8),  // Pct
                Constraint::Min(20),    // Reset time
            ])
            .split(row_rect);

        let pct_color = if bn.remaining_percent >= 60.0 {
            to_ratatui_color(palette::EMERALD)
        } else if bn.remaining_percent >= 25.0 {
            to_ratatui_color(palette::AMBER)
        } else {
            to_ratatui_color(palette::ROSE)
        };

        // Slot 0: Bullet + Label
        let p0 = Paragraph::new(Line::from(vec![
            Span::styled(
                "   • ",
                Style::default().fg(to_ratatui_color(palette::MUTED)),
            ),
            Span::styled(label.as_str(), Style::default().fg(Color::White)),
        ]));
        f.render_widget(p0, row_slots[0]);

        // Slot 1: Window name
        let p1 = Paragraph::new(Span::styled(
            &bn.name,
            Style::default().fg(to_ratatui_color(palette::MUTED)),
        ));
        f.render_widget(p1, row_slots[1]);

        // Slot 2: Pct
        let p2 = Paragraph::new(Span::styled(
            format!("{:>6.1}%", bn.remaining_percent),
            Style::default().fg(pct_color).add_modifier(Modifier::BOLD),
        ));
        f.render_widget(p2, row_slots[2]);

        // Slot 3: Reset
        let p3 = Paragraph::new(Span::styled(
            format!("resets in {}", bn.format_reset_time()),
            Style::default().fg(to_ratatui_color(palette::MUTED)),
        ));
        f.render_widget(p3, row_slots[3]);
    }
}

fn render_tui_statusbar(f: &mut Frame, area: Rect, app: &TuiApp) {
    let spinner_frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let frame_char = spinner_frames[app.frame_tick % spinner_frames.len()];

    let status_line = Line::from(vec![
        Span::styled(
            format!(" {} ", frame_char),
            Style::default().fg(to_ratatui_color(palette::ACCENT_BLUE)),
        ),
        Span::styled(
            if app.is_refreshing {
                "Refreshing quotas across accounts...".to_string()
            } else {
                format!("Auto-refresh in {:>2}s", app.countdown_seconds)
            },
            Style::default().fg(to_ratatui_color(palette::MUTED)),
        ),
        Span::styled(
            "   [r] Refresh now   [q] Quit",
            Style::default().fg(to_ratatui_color(palette::BORDER)),
        ),
    ]);

    let p = Paragraph::new(status_line);
    f.render_widget(p, area);
}
