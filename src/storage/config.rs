use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const TEMPLATE: &str = "\
# luna local config — stays on this machine, never commit it.
# Google OAuth client used by Antigravity / Cloud Code.

[antigravity]
client_id = \"\"
client_secret = \"\"
";

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LunaConfig {
    #[serde(default)]
    pub antigravity: AntigravityOAuth,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AntigravityOAuth {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
}

/// `~/.config/luna/config.toml`
pub fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config/luna/config.toml")
}

pub fn load() -> Result<LunaConfig> {
    let path = config_path();
    if !path.exists() {
        write_template(&path)?;
        bail!(missing_message(&path));
    }
    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

/// Antigravity OAuth client from `~/.config/luna/config.toml`.
pub fn antigravity_oauth() -> Result<(String, String)> {
    let cfg = load()?;
    let id = cfg.antigravity.client_id.trim().to_string();
    let secret = cfg.antigravity.client_secret.trim().to_string();
    if id.is_empty() || secret.is_empty() {
        bail!(missing_message(&config_path()));
    }
    Ok((id, secret))
}

fn missing_message(path: &Path) -> String {
    format!(
        "Antigravity OAuth client is not configured.\n\
         Edit {} and set:\n\n\
         [antigravity]\n\
         client_id = \"....apps.googleusercontent.com\"\n\
         client_secret = \"...\"",
        path.display()
    )
}

fn write_template(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(parent)?.permissions();
            perms.set_mode(0o700);
            let _ = fs::set_permissions(parent, perms);
        }
    }
    fs::write(path, TEMPLATE)?;
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}
