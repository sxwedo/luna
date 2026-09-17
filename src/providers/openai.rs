use super::{Provider, ProviderError};
use crate::domain::account::{Account, ProviderId};
use crate::domain::credential::AuthCredential;
use crate::domain::quota::{AccountQuota, QuotaPeriod, QuotaWindow};
use async_trait::async_trait;
use reqwest::Client;

#[derive(Default, Clone)]
pub struct OpenAiProvider;

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::OpenAI
    }

    fn name(&self) -> &'static str {
        "OpenAI / Codex"
    }

    async fn refresh_token(
        &self,
        _credential: &mut AuthCredential,
        _client: &Client,
    ) -> Result<bool, ProviderError> {
        Ok(false)
    }

    async fn fetch_quota(
        &self,
        account: &Account,
        client: &Client,
    ) -> Result<AccountQuota, ProviderError> {
        let token = match account.credential.token_str() {
            Some(t) => t,
            None => {
                return Err(ProviderError::Authentication(
                    "Missing API key or token".into(),
                ))
            }
        };

        // Probe OpenAI models endpoint to verify key and active status
        let base_url = match &account.credential {
            AuthCredential::ApiKey {
                base_url: Some(url),
                ..
            } => url.clone(),
            _ => "https://api.openai.com/v1".to_string(),
        };

        let resp = client
            .get(format!("{}/models", base_url))
            .bearer_auth(token)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            return Ok(AccountQuota::failed(
                &account.id,
                ProviderId::OpenAI,
                &account.label,
                format!("OpenAI API error {}: {}", status, err_text),
            ));
        }

        // Return a healthy API Key status window
        let windows = vec![QuotaWindow::new(
            "API Key Status",
            QuotaPeriod::Custom,
            100.0,
            None,
        )];

        Ok(AccountQuota::success(
            &account.id,
            ProviderId::OpenAI,
            &account.label,
            Some("OpenAI Platform API".to_string()),
            None,
            windows,
        ))
    }
}
