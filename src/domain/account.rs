use super::credential::AuthCredential;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Supported Provider Identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderId {
    Antigravity,
    OpenAI,
    Claude,
    Copilot,
    Zhipu,
    Grok,
    Custom,
}

/// How an account is added for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginKind {
    OAuth,
    ApiKey,
}

impl ProviderId {
    pub const ALL: [ProviderId; 6] = [
        Self::Antigravity,
        Self::OpenAI,
        Self::Claude,
        Self::Copilot,
        Self::Zhipu,
        Self::Grok,
    ];

    /// Providers with working login flows. Others are parsed/filtered but not
    /// yet offered in the interactive picker.
    pub const LOGIN_SUPPORTED: [ProviderId; 3] = [Self::Antigravity, Self::Grok, Self::Zhipu];

    pub fn supports_login(self) -> bool {
        Self::LOGIN_SUPPORTED.contains(&self)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Antigravity => "antigravity",
            Self::OpenAI => "openai",
            Self::Claude => "claude",
            Self::Copilot => "copilot",
            Self::Zhipu => "zhipu",
            Self::Grok => "grok",
            Self::Custom => "custom",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Antigravity => "Google Antigravity",
            Self::OpenAI => "OpenAI / Codex",
            Self::Claude => "Anthropic Claude",
            Self::Copilot => "GitHub Copilot",
            Self::Zhipu => "智谱 GLM",
            Self::Grok => "Grok / xAI",
            Self::Custom => "Custom",
        }
    }

    pub fn login_kind(self) -> LoginKind {
        match self {
            Self::Antigravity | Self::Grok => LoginKind::OAuth,
            Self::OpenAI | Self::Claude | Self::Copilot | Self::Zhipu | Self::Custom => {
                LoginKind::ApiKey
            }
        }
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for ProviderId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "antigravity" | "google" | "gemini" => Ok(Self::Antigravity),
            "openai" | "codex" => Ok(Self::OpenAI),
            "claude" | "anthropic" => Ok(Self::Claude),
            "copilot" | "github" => Ok(Self::Copilot),
            "zhipu" | "glm" | "bigmodel" | "zai" | "智谱" => Ok(Self::Zhipu),
            "grok" | "xai" | "x.ai" => Ok(Self::Grok),
            "custom" => Ok(Self::Custom),
            other => Err(format!("Unknown provider: {}", other)),
        }
    }
}

/// An account entry managed by luna.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Account {
    /// Unique identifier for this account within luna (e.g. "antigravity:user@gmail.com").
    pub id: String,
    /// The LLM provider.
    pub provider: ProviderId,
    /// Human-friendly display label or user email.
    pub label: String,
    /// The authentication credential.
    pub credential: AuthCredential,
    /// Whether this account is active/enabled for checks.
    pub enabled: bool,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last successful update timestamp.
    pub updated_at: DateTime<Utc>,
}

impl Account {
    pub fn new(
        id: impl Into<String>,
        provider: ProviderId,
        label: impl Into<String>,
        credential: AuthCredential,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: id.into(),
            provider,
            label: label.into(),
            credential,
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn identity_id(provider: ProviderId, identity: &str) -> String {
        format!("{}:{}", provider.as_str(), identity.trim().to_lowercase())
    }

    pub fn is_placeholder(&self) -> bool {
        self.id.ends_with(":imported-keychain") || self.label.contains("Imported from IDE")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_picker_only_offers_supported_providers() {
        assert_eq!(
            ProviderId::LOGIN_SUPPORTED,
            [ProviderId::Antigravity, ProviderId::Grok, ProviderId::Zhipu]
        );
        assert!(ProviderId::Antigravity.supports_login());
        assert!(ProviderId::Grok.supports_login());
        assert!(ProviderId::Zhipu.supports_login());
        assert!(!ProviderId::OpenAI.supports_login());
        assert!(!ProviderId::Claude.supports_login());
        assert!(!ProviderId::Copilot.supports_login());
    }
}
