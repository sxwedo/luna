use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Authentication credential for an account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthCredential {
    /// Full OAuth 2.0 credentials with access and refresh tokens.
    OAuth {
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<DateTime<Utc>>,
        client_id: Option<String>,
        client_secret: Option<String>,
        project_id: Option<String>,
    },
    /// Direct API Key authentication.
    ApiKey {
        key: String,
        base_url: Option<String>,
    },
    /// Reference to a native system Keychain entry (e.g. macOS Keychain).
    NativeKeychain {
        service: String,
        account: String,
        cached_access_token: Option<String>,
        expires_at: Option<DateTime<Utc>>,
    },
}

impl AuthCredential {
    /// Check if the token is expired or will expire within `leeway_seconds`.
    pub fn is_expired(&self, leeway_seconds: i64) -> bool {
        match self {
            Self::OAuth { expires_at, .. } => {
                if let Some(exp) = expires_at {
                    let threshold = Utc::now() + chrono::Duration::seconds(leeway_seconds);
                    exp <= &threshold
                } else {
                    false
                }
            }
            Self::NativeKeychain { expires_at, .. } => {
                if let Some(exp) = expires_at {
                    let threshold = Utc::now() + chrono::Duration::seconds(leeway_seconds);
                    exp <= &threshold
                } else {
                    false
                }
            }
            Self::ApiKey { .. } => false,
        }
    }

    /// Return the active access token or API key as string slice if available.
    pub fn token_str(&self) -> Option<&str> {
        match self {
            Self::OAuth { access_token, .. } => Some(access_token),
            Self::ApiKey { key, .. } => Some(key),
            Self::NativeKeychain {
                cached_access_token,
                ..
            } => cached_access_token.as_deref(),
        }
    }
}
