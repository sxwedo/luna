#[path = "antigravity.rs"]
pub mod antigravity;
#[path = "claude.rs"]
pub mod claude;
#[path = "grok.rs"]
pub mod grok;
#[path = "openai.rs"]
pub mod openai;
#[path = "zhipu.rs"]
pub mod zhipu;

use crate::domain::account::{Account, ProviderId};
use crate::domain::credential::AuthCredential;
use crate::domain::quota::AccountQuota;
use async_trait::async_trait;
use reqwest::Client;
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("Authentication failed: {0}")]
    Authentication(String),

    #[error("Token expired and cannot be refreshed: {0}")]
    TokenExpired(String),

    #[error("Quota unavailable or forbidden (403): {0}")]
    QuotaUnavailable(String),

    #[error("Network HTTP error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Serialization / parsing error: {0}")]
    Serialization(String),

    #[error("General provider error: {0}")]
    Other(String),
}

/// Common trait implemented by each LLM quota provider.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Identifier for this provider.
    fn id(&self) -> ProviderId;

    /// Human-friendly provider name.
    fn name(&self) -> &'static str;

    /// Fetch quota windows for the given credential.
    async fn fetch_quota(
        &self,
        account: &Account,
        client: &Client,
    ) -> Result<AccountQuota, ProviderError>;

    /// Refresh token if applicable, updating the credential in-place.
    /// Returns Ok(true) if token was refreshed, Ok(false) if refresh not needed, or Err.
    async fn refresh_token(
        &self,
        credential: &mut AuthCredential,
        client: &Client,
    ) -> Result<bool, ProviderError>;
}

/// Provider factory/registry for resolving provider implementations.
pub struct ProviderRegistry;

impl ProviderRegistry {
    pub fn get(provider_id: ProviderId) -> Arc<dyn Provider> {
        match provider_id {
            ProviderId::Antigravity => Arc::new(antigravity::AntigravityProvider),
            ProviderId::OpenAI => Arc::new(openai::OpenAiProvider),
            ProviderId::Claude => Arc::new(claude::ClaudeProvider),
            ProviderId::Zhipu => Arc::new(zhipu::ZhipuProvider),
            ProviderId::Grok => Arc::new(grok::GrokProvider),
            ProviderId::Copilot | ProviderId::Custom => Arc::new(openai::OpenAiProvider),
        }
    }
}
