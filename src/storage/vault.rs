use crate::domain::account::{Account, ProviderId};
use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// On-disk representation of accounts vault.
#[derive(Debug, Default, Serialize, Deserialize)]
struct VaultDocument {
    version: u32,
    accounts: BTreeMap<String, Account>,
}

/// The Account Vault manages persisted accounts.
#[derive(Debug, Clone)]
pub struct AccountVault {
    path: PathBuf,
}

impl AccountVault {
    /// Creates a vault with default storage path in user configuration directory.
    pub fn default_vault() -> Result<Self> {
        let path = Self::default_path()?;
        Ok(Self { path })
    }

    /// Custom path vault.
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Resolve the luna config path, migrating a legacy quotactl vault if needed.
    pub fn default_path() -> Result<PathBuf> {
        let luna = Self::luna_path()?;
        if luna.exists() {
            return Ok(luna);
        }
        for legacy in Self::legacy_paths() {
            if legacy.exists() {
                if let Some(parent) = luna.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if fs::copy(&legacy, &luna).is_ok() {
                    return Ok(luna);
                }
                return Ok(legacy);
            }
        }
        Ok(luna)
    }

    fn luna_path() -> Result<PathBuf> {
        if let Some(proj_dirs) = ProjectDirs::from("dev", "sxwedo", "luna") {
            Ok(proj_dirs.config_dir().join("accounts.json"))
        } else {
            let home = std::env::var("HOME").context("HOME environment variable not found")?;
            Ok(PathBuf::from(home)
                .join(".config")
                .join("luna")
                .join("accounts.json"))
        }
    }

    fn legacy_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(proj_dirs) = ProjectDirs::from("com", "quotactl", "quotactl") {
            paths.push(proj_dirs.config_dir().join("accounts.json"));
        }
        if let Ok(home) = std::env::var("HOME") {
            let home = PathBuf::from(home);
            paths.push(home.join(".config/quotactl/accounts.json"));
            paths
                .push(home.join("Library/Application Support/com.quotactl.quotactl/accounts.json"));
        }
        paths
    }

    fn read_document(&self) -> Result<VaultDocument> {
        if !self.path.exists() {
            return Ok(VaultDocument {
                version: 1,
                accounts: BTreeMap::new(),
            });
        }
        let content = fs::read_to_string(&self.path)
            .with_context(|| format!("Failed to read vault from {:?}", self.path))?;
        let doc: VaultDocument = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse vault file {:?}", self.path))?;
        Ok(doc)
    }

    fn write_document(&self, doc: &VaultDocument) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directory {:?}", parent))?;
            #[cfg(unix)]
            {
                let mut perms = fs::metadata(parent)?.permissions();
                perms.set_mode(0o700);
                let _ = fs::set_permissions(parent, perms);
            }
        }

        let json = serde_json::to_string_pretty(doc)?;
        let tmp_path = self.path.with_extension("tmp");
        {
            let mut file = File::create(&tmp_path)?;
            #[cfg(unix)]
            {
                let mut perms = file.metadata()?.permissions();
                perms.set_mode(0o600);
                file.set_permissions(perms)?;
            }
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
        }
        fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }

    /// Returns all accounts sorted by ID.
    pub fn list(&self) -> Result<Vec<Account>> {
        let doc = self.read_document()?;
        Ok(doc.accounts.into_values().collect())
    }

    /// Find an account by ID or label.
    pub fn find(&self, query: &str) -> Result<Option<Account>> {
        let doc = self.read_document()?;
        if let Some(acc) = doc.accounts.get(query) {
            return Ok(Some(acc.clone()));
        }
        let matched = doc
            .accounts
            .values()
            .find(|a| a.label == query || a.id == query);
        Ok(matched.cloned())
    }

    /// Save or update an account.
    pub fn save(&self, account: Account) -> Result<()> {
        self.upsert(account, None)
    }

    /// Insert/update an account, optionally dropping a previous ID (used when migrating placeholders).
    pub fn upsert(&self, account: Account, previous_id: Option<&str>) -> Result<()> {
        let mut doc = self.read_document()?;
        if let Some(old_id) = previous_id {
            if old_id != account.id {
                doc.accounts.remove(old_id);
            }
        }
        doc.accounts.insert(account.id.clone(), account);
        self.write_document(&doc)?;
        Ok(())
    }

    /// Remove an account by ID.
    pub fn remove(&self, id: &str) -> Result<bool> {
        let mut doc = self.read_document()?;
        let removed = doc.accounts.remove(id).is_some();
        if removed {
            self.write_document(&doc)?;
        }
        Ok(removed)
    }

    /// Filter accounts by provider.
    pub fn list_by_provider(&self, provider: ProviderId) -> Result<Vec<Account>> {
        let accounts = self.list()?;
        Ok(accounts
            .into_iter()
            .filter(|a| a.provider == provider)
            .collect())
    }
}
