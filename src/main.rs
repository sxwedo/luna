use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, ContentArrangement, Table};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Password, Select};
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;

use luna::domain::account::{Account, LoginKind, ProviderId};
use luna::domain::credential::AuthCredential;
use luna::domain::quota::AccountQuota;
use luna::providers::antigravity::AntigravityProvider;
use luna::providers::ProviderRegistry;
use luna::storage::vault::AccountVault;
use luna::ui::card::render_quota_cards_animated;
use luna::ui::colors::{palette, rgb};
use luna::ui::table::print_quota_overview;
use luna::ui::tui::run_watch_tui;

#[derive(Parser)]
#[command(name = "luna")]
#[command(author = "sxwedo")]
#[command(version = "0.1.0")]
#[command(about = "The remaining light of your LLM quotas", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Filter by provider (e.g. antigravity, openai, claude)
    #[arg(short, long, global = true)]
    provider: Option<String>,

    /// Output format (cards, table, or json)
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Cards, global = true)]
    format: OutputFormat,

    /// Live watch mode with automatic refresh
    #[arg(short, long, global = true)]
    watch: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Default)]
enum OutputFormat {
    #[default]
    Cards,
    Table,
    Json,
}

#[derive(Subcommand)]
enum Commands {
    /// Check and monitor quota for all or filtered accounts (default action)
    #[command(alias = "usage", alias = "check")]
    Status {
        /// Specific account ID or label to check
        #[arg(short, long)]
        account: Option<String>,
    },

    /// Interactive login. Omit --provider to pick from a list.
    Login {
        /// Provider to log into (antigravity, zhipu)
        #[arg(short, long)]
        provider: Option<String>,
    },

    /// Sniff and import credentials from local installed IDEs (e.g. macOS Keychain)
    #[command(alias = "discover")]
    Sniff,

    /// Manually add an account with an API Key or Token
    Add {
        /// Provider name (antigravity, openai, claude)
        #[arg(short, long)]
        provider: String,

        /// Account label or description
        #[arg(short, long)]
        label: String,

        /// API Key or access token
        #[arg(short, long)]
        key: String,

        /// Optional OAuth refresh token
        #[arg(short, long)]
        refresh: Option<String>,
    },

    /// List all configured accounts
    #[command(alias = "ls")]
    List,

    /// Log out an account. Omit --account to pick interactively.
    #[command(alias = "out")]
    Logout {
        /// Account ID or label to log out
        #[arg(short, long)]
        account: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let vault = AccountVault::default_vault()?;

    match cli.command.unwrap_or(Commands::Status { account: None }) {
        Commands::Status { account } => {
            handle_status(
                &vault,
                cli.provider.as_deref(),
                account.as_deref(),
                cli.format,
                cli.watch,
            )
            .await?;
        }
        Commands::Login { provider } => {
            handle_login(&vault, provider.as_deref()).await?;
        }
        Commands::Sniff => {
            handle_sniff(&vault).await?;
        }
        Commands::Add {
            provider,
            label,
            key,
            refresh,
        } => {
            handle_add(&vault, &provider, &label, &key, refresh.as_deref()).await?;
        }
        Commands::List => {
            handle_list(&vault).await?;
        }
        Commands::Logout { account } => {
            handle_logout(&vault, account.as_deref()).await?;
        }
    }

    Ok(())
}

fn http_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .context("Failed to construct HTTP client")
}

fn prompt_provider() -> Result<ProviderId> {
    let labels: Vec<String> = ProviderId::LOGIN_SUPPORTED
        .iter()
        .map(|provider| {
            let kind = match provider.login_kind() {
                LoginKind::OAuth => "浏览器登录",
                LoginKind::ApiKey => "API Key",
            };
            format!("{}  ({})", provider.display_name(), kind)
        })
        .collect();

    let idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("选择要登录的 Provider  (↑↓ 选择, Enter 确认)")
        .items(&labels)
        .default(0)
        .interact()?;
    Ok(ProviderId::LOGIN_SUPPORTED[idx])
}

async fn migrate_placeholder_accounts(vault: &AccountVault, client: &Client) -> Result<()> {
    for acc in vault.list()? {
        if acc.provider != ProviderId::Antigravity || !acc.is_placeholder() {
            continue;
        }
        let Some(token) = acc.credential.token_str() else {
            continue;
        };
        let Some(email) = AntigravityProvider::fetch_user_email(client, token).await else {
            continue;
        };
        let old_id = acc.id.clone();
        let new_id = Account::identity_id(ProviderId::Antigravity, &email);
        if vault.find(&new_id)?.is_some() && new_id != old_id {
            let _ = vault.remove(&old_id);
            continue;
        }
        let mut updated = acc;
        updated.id = new_id;
        updated.label = email;
        vault.upsert(updated, Some(&old_id))?;
    }
    Ok(())
}

async fn handle_status(
    vault: &AccountVault,
    filter_provider: Option<&str>,
    filter_account: Option<&str>,
    format: OutputFormat,
    watch: bool,
) -> Result<()> {
    let client = http_client()?;
    migrate_placeholder_accounts(vault, &client).await?;

    let mut accounts = vault.list()?;

    if accounts.is_empty() {
        if let Ok(Some(sniffed)) = AntigravityProvider::sniff_installed_credentials(&client).await {
            if format != OutputFormat::Json {
                println!(
                    "{}",
                    format!("Auto-discovered Antigravity account: {}", sniffed.label).dimmed()
                );
            }
            let _ = vault.save(sniffed.clone());
            accounts.push(sniffed);
        }
    }

    if let Some(prov_str) = filter_provider {
        let p_id: ProviderId = prov_str.parse().map_err(|e: String| anyhow::anyhow!(e))?;
        accounts.retain(|a| a.provider == p_id);
    }

    if let Some(acc_str) = filter_account {
        accounts.retain(|a| a.id == acc_str || a.label.contains(acc_str));
    }

    if accounts.is_empty() {
        if format == OutputFormat::Json {
            println!("[]");
        } else {
            println!("{}", "No matching accounts found.".yellow());
            println!(
                "  To add an account:  {} or {}",
                "luna login".cyan(),
                "luna sniff".cyan()
            );
        }
        return Ok(());
    }

    // Animated fetching indicator for interactive terminals
    if format != OutputFormat::Json && !watch {
        use std::io::Write;
        print!(
            "  {} {}",
            rgb("⠋", palette::ACCENT_BLUE),
            rgb("Refreshing quotas...", palette::MUTED)
        );
        let _ = std::io::stdout().flush();
    }

    let client = Arc::new(client);
    let mut join_set = JoinSet::new();

    for mut account in accounts {
        let client_clone = Arc::clone(&client);
        join_set.spawn(async move {
            let provider_impl = ProviderRegistry::get(account.provider);

            // Automatically check and refresh token if expired
            let mut refreshed = false;
            if account.credential.is_expired(300) {
                if let Ok(true) = provider_impl
                    .refresh_token(&mut account.credential, &client_clone)
                    .await
                {
                    refreshed = true;
                }
            }

            let quota = match provider_impl.fetch_quota(&account, &client_clone).await {
                Ok(q) => q,
                Err(err) => AccountQuota::failed(
                    &account.id,
                    account.provider,
                    &account.label,
                    err.to_string(),
                ),
            };

            (account, quota, refreshed)
        });
    }

    let mut quotas = Vec::new();
    while let Some(res) = join_set.join_next().await {
        if let Ok((account, quota, refreshed)) = res {
            if refreshed {
                let _ = vault.save(account);
            }
            quotas.push(quota);
        }
    }

    // Clear the fetching indicator line
    if format != OutputFormat::Json && !watch {
        print!("\r\x1b[2K");
    }

    // Sort by provider and account label for consistent layout
    quotas.sort_by(|a, b| {
        a.provider
            .as_str()
            .cmp(b.provider.as_str())
            .then_with(|| a.label.cmp(&b.label))
    });

    if watch {
        // Enter the high-performance 10FPS interactive TUI watch mode with dual-column grid & streaming light
        return run_watch_tui(
            vault.clone(),
            filter_provider.map(str::to_string),
            filter_account.map(str::to_string),
            quotas,
        )
        .await;
    }

    match format {
        OutputFormat::Cards => {
            render_quota_cards_animated(&quotas, None, 0);
        }
        OutputFormat::Table => {
            print_quota_overview(&quotas);
        }
        OutputFormat::Json => {
            let json_out = serde_json::to_string_pretty(&quotas)?;
            println!("{}", json_out);
        }
    }

    Ok(())
}

async fn handle_login(vault: &AccountVault, provider_name: Option<&str>) -> Result<()> {
    let p_id = match provider_name {
        Some(name) => name.parse().map_err(|e: String| anyhow::anyhow!(e))?,
        None => prompt_provider()?,
    };

    if !p_id.supports_login() {
        println!(
            "{}",
            format!(
                "暂不支持登录 {}，当前可用: {}",
                p_id.display_name(),
                ProviderId::LOGIN_SUPPORTED
                    .iter()
                    .map(|p| p.display_name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .yellow()
        );
        return Ok(());
    }

    match p_id.login_kind() {
        LoginKind::OAuth if p_id == ProviderId::Antigravity => {
            let account = AntigravityProvider::login_interactive()
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "{}",
                format!("Logged in as {}", account.label).green().bold()
            );
            vault.save(account)?;
            println!("Account saved.");
        }
        LoginKind::ApiKey => {
            let theme = ColorfulTheme::default();
            let label: String = Input::with_theme(&theme)
                .with_prompt("账号备注 / 邮箱")
                .interact_text()?;
            let key = Password::with_theme(&theme)
                .with_prompt("API Key")
                .interact()?;
            if label.trim().is_empty() || key.trim().is_empty() {
                anyhow::bail!("label and API key are required");
            }
            handle_add(vault, p_id.as_str(), label.trim(), key.trim(), None).await?;
        }
        LoginKind::OAuth => {
            println!(
                "{}",
                format!(
                    "OAuth login for '{}' is not implemented yet.",
                    p_id.display_name()
                )
                .yellow()
            );
        }
    }

    Ok(())
}

async fn handle_sniff(vault: &AccountVault) -> Result<()> {
    println!(
        "{}",
        "Scanning macOS Keychain for Antigravity IDE credentials...".dimmed()
    );

    let client = http_client()?;
    match AntigravityProvider::sniff_installed_credentials(&client).await {
        Ok(Some(account)) => {
            println!(
                "{}",
                format!("Found IDE account: {}", account.label)
                    .green()
                    .bold()
            );
            let _ = vault.remove("antigravity:imported-keychain");
            vault.save(account)?;
            println!("Account registered. Run `luna status` to check quotas.");
        }
        Ok(None) => {
            println!(
                "{}",
                "No Antigravity credentials found in macOS Keychain.".yellow()
            );
            println!("  Make sure Google Antigravity IDE is installed and signed in.");
        }
        Err(e) => {
            println!("{}", format!("Error scanning Keychain: {}", e).red());
        }
    }

    Ok(())
}

async fn handle_add(
    vault: &AccountVault,
    provider_str: &str,
    label: &str,
    key: &str,
    refresh_token: Option<&str>,
) -> Result<()> {
    let provider: ProviderId = provider_str
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;

    let credential = if let Some(refresh) = refresh_token {
        AuthCredential::OAuth {
            access_token: key.to_string(),
            refresh_token: Some(refresh.to_string()),
            expires_at: None,
            client_id: None,
            client_secret: None,
            project_id: None,
        }
    } else {
        AuthCredential::ApiKey {
            key: key.to_string(),
            base_url: None,
        }
    };

    let id = format!(
        "{}:{}",
        provider.as_str(),
        label.to_lowercase().replace(' ', "-")
    );
    let account = Account::new(id, provider, label, credential);

    vault.save(account)?;
    println!(
        "{}",
        format!(
            " Successfully added account '{}' for provider '{}'",
            label, provider
        )
        .green()
    );

    Ok(())
}

async fn handle_list(vault: &AccountVault) -> Result<()> {
    if let Ok(client) = http_client() {
        let _ = migrate_placeholder_accounts(vault, &client).await;
    }
    let accounts = vault.list()?;
    if accounts.is_empty() {
        println!(
            "{}",
            "No accounts configured. Use `luna login` or `luna add`.".dimmed()
        );
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Disabled)
        .set_header(vec![
            Cell::new("ID").fg(Color::Cyan),
            Cell::new("Provider").fg(Color::Magenta),
            Cell::new("Label").fg(Color::Blue),
            Cell::new("Type").fg(Color::White),
            Cell::new("Status").fg(Color::Green),
            Cell::new("Created").fg(Color::White),
        ]);

    for acc in accounts {
        let cred_type = match &acc.credential {
            AuthCredential::OAuth { .. } => "OAuth 2.0 (Token)",
            AuthCredential::ApiKey { .. } => "API Key",
            AuthCredential::NativeKeychain { .. } => "Keychain",
        };

        let (status, status_color) = if acc.enabled {
            ("Enabled", Color::Green)
        } else {
            ("Disabled", Color::DarkGrey)
        };

        table.add_row(vec![
            Cell::new(acc.id),
            Cell::new(acc.provider.as_str()),
            Cell::new(acc.label),
            Cell::new(cred_type),
            Cell::new(status).fg(status_color),
            Cell::new(acc.created_at.format("%Y-%m-%d %H:%M").to_string()),
        ]);
    }

    println!("\n{}", "=== Configured Accounts ===".bold().cyan());
    println!("{}", table);
    println!();

    Ok(())
}

async fn handle_logout(vault: &AccountVault, account: Option<&str>) -> Result<()> {
    let accounts = vault.list()?;
    if accounts.is_empty() {
        println!(
            "{}",
            "No accounts to log out. Use `luna login` first.".dimmed()
        );
        return Ok(());
    }

    let theme = ColorfulTheme::default();
    let target = match account {
        // Non-interactive path: match by exact ID, then by label substring.
        Some(query) => accounts
            .iter()
            .find(|a| a.id == query)
            .or_else(|| accounts.iter().find(|a| a.label == query))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Account '{query}' not found"))?,
        None => {
            let labels: Vec<String> = accounts
                .iter()
                .map(|a| {
                    let kind = match &a.credential {
                        AuthCredential::OAuth { .. } => "OAuth",
                        AuthCredential::ApiKey { .. } => "API Key",
                        AuthCredential::NativeKeychain { .. } => "Keychain",
                    };
                    format!("{}  ·  {}  ·  {}", a.label, a.provider, kind)
                })
                .collect();
            let idx = Select::with_theme(&theme)
                .with_prompt("选择要退出的账号  (↑↓ 选择, Enter 确认)")
                .items(&labels)
                .default(0)
                .interact()?;
            accounts[idx].clone()
        }
    };

    let confirmed = Confirm::with_theme(&theme)
        .with_prompt(format!("退出账号 {}? (凭证将被删除)", target.label))
        .default(false)
        .interact()?;
    if !confirmed {
        println!("{}", "已取消".dimmed());
        return Ok(());
    }

    if vault.remove(&target.id)? {
        println!(
            "{}",
            format!("已退出账号 {} ({})", target.label, target.id).green()
        );
    } else {
        println!("{}", format!("账号 {} 已不存在", target.label).yellow());
    }
    Ok(())
}
