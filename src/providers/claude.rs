use super::{Provider, ProviderError};
use crate::domain::account::{Account, ProviderId};
use crate::domain::credential::AuthCredential;
use crate::domain::quota::{AccountQuota, QuotaPeriod, QuotaWindow};
use async_trait::async_trait;
use reqwest::Client;

#[derive(Default, Clone)]
pub struct ClaudeProvider;

#[async_trait]
impl Provider for ClaudeProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Claude
    }

    fn name(&self) -> &'static str {
        "Anthropic Claude"
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
                    "Missing Claude API key".into(),
                ))
            }
        };

        // Probe Anthropic API
        let resp = client
            .get("https://api.anthropic.com/v1/models")
            .header("x-api-key", token)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            return Ok(AccountQuota::failed(
                &account.id,
                ProviderId::Claude,
                &account.label,
                format!("Anthropic API error {}: {}", status, err_text),
            ));
        }

        let windows = vec![QuotaWindow::new(
            "Claude API Status",
            QuotaPeriod::Custom,
            100.0,
            None,
        )];

        Ok(AccountQuota::success(
            &account.id,
            ProviderId::Claude,
            &account.label,
            Some("Anthropic API Tier".to_string()),
            None,
            windows,
        ))
    }
}
