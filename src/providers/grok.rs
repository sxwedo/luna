use super::{Provider, ProviderError};
use crate::domain::account::{Account, ProviderId};
use crate::domain::credential::AuthCredential;
use crate::domain::quota::{AccountQuota, QuotaPeriod, QuotaWindow};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use rand::RngCore;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Public Grok CLI OIDC client (PKCE, no secret). Same id the official CLI uses.
const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const AUTH_URL: &str = "https://auth.x.ai/oauth2/authorize";
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const SETTINGS_URL: &str = "https://cli-chat-proxy.grok.com/v1/settings";
const TOKEN_AUTH: &str = "xai-grok-cli";
const SCOPES: &str = "openid profile email offline_access grok-cli:access api:access";

#[derive(Default, Clone)]
pub struct GrokProvider;

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<i64>,
}

fn grok_auth_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".grok/auth.json")
}

fn json_number(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))
        .or_else(|| value.as_u64().map(|n| n as f64))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn jwt_claim(token: &str, claim: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let json: Value = serde_json::from_slice(&decoded).ok()?;
    json.get(claim)
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            json.get(claim)
                .and_then(Value::as_i64)
                .map(|n| n.to_string())
        })
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Parse Grok CLI billing JSON into remaining-percent windows.
pub fn parse_grok_billing(root: &Value) -> Vec<QuotaWindow> {
    let Some(config) = root.get("config").filter(|v| v.is_object()) else {
        return Vec::new();
    };
    let period = config.get("currentPeriod");
    let (period_kind, name) = match period.and_then(|p| p.get("type")).and_then(Value::as_str) {
        Some("USAGE_PERIOD_TYPE_WEEKLY") => (QuotaPeriod::Weekly7D, "Grok Weekly"),
        Some("USAGE_PERIOD_TYPE_MONTHLY") => (QuotaPeriod::Monthly, "Grok Monthly"),
        _ => (QuotaPeriod::Custom, "Grok Credits"),
    };

    let used = json_number(config.get("creditUsagePercent")).unwrap_or(0.0);
    let remaining = (100.0 - used).clamp(0.0, 100.0);
    let resets_at = period
        .and_then(|p| p.get("end"))
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)
        .or_else(|| {
            config
                .get("billingPeriodEnd")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339)
        });

    let mut windows = vec![QuotaWindow::new(name, period_kind, remaining, resets_at)];

    if let Some(products) = config.get("productUsage").and_then(Value::as_array) {
        for product in products {
            let label = product.get("product").and_then(Value::as_str).unwrap_or("");
            let Some(used_pct) = json_number(product.get("usagePercent")) else {
                continue;
            };
            let pretty = match label {
                "GrokBuild" => "Grok Build",
                "GrokChat" => "Grok Chat",
                "GrokImagine" => "Grok Imagine",
                other if !other.is_empty() => other,
                _ => continue,
            };
            windows.push(QuotaWindow::new(
                pretty,
                period_kind,
                (100.0 - used_pct).clamp(0.0, 100.0),
                resets_at,
            ));
        }
    }

    let cap = json_number(
        config
            .get("onDemandCap")
            .and_then(|v| v.get("val"))
            .or_else(|| config.get("onDemandCap")),
    );
    if let Some(cap) = cap {
        if cap > 0.0 {
            let used_extra = json_number(
                config
                    .get("onDemandUsed")
                    .and_then(|v| v.get("val"))
                    .or_else(|| config.get("onDemandUsed")),
            )
            .unwrap_or(0.0);
            let remaining_extra = ((cap - used_extra) / cap * 100.0).clamp(0.0, 100.0);
            windows.push(QuotaWindow::new(
                "Grok Extra",
                QuotaPeriod::Custom,
                remaining_extra,
                None,
            ));
        }
    }

    windows
}

impl GrokProvider {
    pub fn new() -> Self {
        Self
    }

    /// Import the Grok CLI session from `~/.grok/auth.json`.
    pub fn sniff_installed_credentials() -> Result<Option<Account>, ProviderError> {
        let path = grok_auth_path();
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&path)
            .map_err(|e| ProviderError::Authentication(format!("read {}: {e}", path.display())))?;
        let root: Value = serde_json::from_str(&raw)
            .map_err(|e| ProviderError::Authentication(format!("parse {}: {e}", path.display())))?;
        let Some(entry) = pick_auth_entry(&root) else {
            return Ok(None);
        };
        Ok(Some(account_from_entry(entry)?))
    }

    pub async fn login_interactive() -> Result<Account, ProviderError> {
        let mut verifier_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut verifier_bytes);
        let code_verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
        let mut hasher = Sha256::new();
        hasher.update(code_verifier.as_bytes());
        let code_challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

        let mut state_bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut state_bytes);
        let state = URL_SAFE_NO_PAD.encode(state_bytes);
        let nonce = {
            let mut n = [0u8; 16];
            rand::thread_rng().fill_bytes(&mut n);
            URL_SAFE_NO_PAD.encode(n)
        };

        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| {
            ProviderError::Other(format!("Failed to bind loopback for Grok OAuth: {e}"))
        })?;
        let port = listener
            .local_addr()
            .map_err(|e| ProviderError::Other(e.to_string()))?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");

        let auth_url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&nonce={}",
            AUTH_URL,
            urlencoding(CLIENT_ID),
            urlencoding(&redirect_uri),
            urlencoding(SCOPES),
            code_challenge,
            state,
            nonce
        );

        println!("\n  === Grok OAuth Login ===");
        println!("  Open this URL in your browser:\n");
        println!("  \x1b[36m{auth_url}\x1b[0m\n");
        println!("  Waiting on {redirect_uri} ...");
        println!(
            "  Tip: the consent page is on accounts.x.ai. If Chrome shows ERR_TUNNEL_CONNECTION_FAILED,\n  \
             set accounts.x.ai / *.x.ai to DIRECT in the proxy, or run `grok login` then `luna sniff`."
        );

        let auth_code = tokio::select! {
            res = wait_for_oauth_code(listener, &state) => res?,
            _ = tokio::time::sleep(Duration::from_secs(180)) => {
                return Err(ProviderError::Other("Grok OAuth timed out after 3 minutes".into()));
            }
        };

        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(ProviderError::Network)?;

        let token_resp: TokenResponse = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", auth_code.as_str()),
                ("redirect_uri", redirect_uri.as_str()),
                ("code_verifier", code_verifier.as_str()),
            ])
            .send()
            .await?
            .error_for_status()
            .map_err(|e| ProviderError::Authentication(format!("Grok token exchange failed: {e}")))?
            .json()
            .await?;

        let expires_at = token_resp
            .expires_in
            .map(|sec| Utc::now() + chrono::Duration::seconds(sec.saturating_sub(60)));
        let email = token_resp
            .id_token
            .as_deref()
            .and_then(|t| jwt_claim(t, "email"))
            .or_else(|| jwt_claim(&token_resp.access_token, "email"))
            .unwrap_or_else(|| "grok-user".to_string());

        Ok(Account::new(
            Account::identity_id(ProviderId::Grok, &email),
            ProviderId::Grok,
            email,
            AuthCredential::OAuth {
                access_token: token_resp.access_token,
                refresh_token: token_resp.refresh_token,
                expires_at,
                client_id: Some(CLIENT_ID.to_string()),
                client_secret: None,
                project_id: None,
            },
        ))
    }
}

fn pick_auth_entry(root: &Value) -> Option<&Value> {
    let obj = root.as_object()?;
    let modern: Vec<&Value> = obj
        .iter()
        .filter(|(k, _)| k.starts_with("https://auth.x.ai::"))
        .map(|(_, v)| v)
        .collect();
    if let Some(first) = modern.first() {
        return Some(*first);
    }
    obj.get("https://accounts.x.ai/sign-in")
}

fn account_from_entry(entry: &Value) -> Result<Account, ProviderError> {
    let token = entry
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| ProviderError::Authentication("Grok auth.json missing access token".into()))?
        .trim();
    if token.is_empty() {
        return Err(ProviderError::Authentication(
            "Grok auth.json has an empty token".into(),
        ));
    }
    let refresh = entry
        .get("refresh_token")
        .or_else(|| entry.get("refresh"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let email = entry
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| jwt_claim(token, "email"))
        .unwrap_or_else(|| "grok-user".to_string());
    let expires_at = entry
        .get("expires_at")
        .or_else(|| entry.get("expires"))
        .and_then(Value::as_str)
        .and_then(parse_rfc3339);
    let client_id = entry
        .get("oidc_client_id")
        .and_then(Value::as_str)
        .unwrap_or(CLIENT_ID)
        .to_string();

    Ok(Account::new(
        Account::identity_id(ProviderId::Grok, &email),
        ProviderId::Grok,
        email,
        AuthCredential::OAuth {
            access_token: token.to_string(),
            refresh_token: refresh,
            expires_at,
            client_id: Some(client_id),
            client_secret: None,
            project_id: None,
        },
    ))
}

fn urlencoding(input: &str) -> String {
    url::form_urlencoded::byte_serialize(input.as_bytes()).collect()
}

async fn wait_for_oauth_code(
    listener: TcpListener,
    expected_state: &str,
) -> Result<String, ProviderError> {
    loop {
        let (mut socket, _) = listener
            .accept()
            .await
            .map_err(|e| ProviderError::Other(format!("Socket accept error: {e}")))?;
        let mut buf = [0u8; 4096];
        let n = socket
            .read(&mut buf)
            .await
            .map_err(|e| ProviderError::Other(e.to_string()))?;
        let request = String::from_utf8_lossy(&buf[..n]);
        if let Some(path) = request
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
        {
            if path.starts_with("/callback") {
                let full_url = format!("http://127.0.0.1{path}");
                if let Ok(parsed) = url::Url::parse(&full_url) {
                    let mut code = None;
                    let mut state = None;
                    for (k, v) in parsed.query_pairs() {
                        if k == "code" {
                            code = Some(v.into_owned());
                        } else if k == "state" {
                            state = Some(v.into_owned());
                        }
                    }
                    if state.as_deref() == Some(expected_state) {
                        if let Some(code) = code {
                            let body = "<html><body><h2>Grok login successful</h2><p>You can close this tab.</p></body></html>";
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = socket.write_all(response.as_bytes()).await;
                            return Ok(code);
                        }
                    }
                }
            }
        }
        let body = "Invalid or missing OAuth state/code";
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
    }
}

fn grok_headers(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    req.bearer_auth(token)
        .header("X-XAI-Token-Auth", TOKEN_AUTH)
        .header("Accept", "application/json")
        .header("User-Agent", "xai-grok-cli")
}

async fn fetch_billing(
    client: &Client,
    token: &str,
) -> Result<(Vec<QuotaWindow>, Option<String>), ProviderError> {
    let billing = grok_headers(client.get(BILLING_URL), token)
        .timeout(Duration::from_secs(12))
        .send()
        .await?;
    if billing.status() == reqwest::StatusCode::UNAUTHORIZED
        || billing.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Err(ProviderError::TokenExpired(format!(
            "Grok billing HTTP {}",
            billing.status()
        )));
    }
    if !billing.status().is_success() {
        let status = billing.status();
        let text = billing.text().await.unwrap_or_default();
        return Err(ProviderError::Other(format!(
            "Grok billing HTTP {status}: {text}"
        )));
    }
    let body: Value = billing
        .json()
        .await
        .map_err(|e| ProviderError::Serialization(format!("Grok billing JSON: {e}")))?;
    let windows = parse_grok_billing(&body);
    if windows.is_empty() {
        return Err(ProviderError::Other(
            "Grok billing returned no quota windows".into(),
        ));
    }

    let plan = match grok_headers(client.get(SETTINGS_URL), token)
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => resp.json::<Value>().await.ok().and_then(|v| {
            v.get("subscription_tier_display")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        }),
        _ => None,
    };
    Ok((windows, plan))
}

#[async_trait]
impl Provider for GrokProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Grok
    }

    fn name(&self) -> &'static str {
        "Grok / xAI"
    }

    async fn refresh_token(
        &self,
        credential: &mut AuthCredential,
        client: &Client,
    ) -> Result<bool, ProviderError> {
        if !credential.is_expired(300) {
            return Ok(false);
        }
        let (refresh, client_id) = match credential {
            AuthCredential::OAuth {
                refresh_token: Some(refresh),
                client_id,
                ..
            } => (
                refresh.clone(),
                client_id
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| CLIENT_ID.to_string()),
            ),
            _ => return Ok(false),
        };

        let resp = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", client_id.as_str()),
                ("refresh_token", refresh.as_str()),
            ])
            .send()
            .await?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TokenExpired(format!(
                "Grok token refresh failed: {body}"
            )));
        }
        let tokens: TokenResponse = resp.json().await?;
        if let AuthCredential::OAuth {
            access_token,
            refresh_token,
            expires_at,
            ..
        } = credential
        {
            *access_token = tokens.access_token;
            if let Some(rotated) = tokens.refresh_token {
                *refresh_token = Some(rotated);
            }
            *expires_at = tokens
                .expires_in
                .map(|sec| Utc::now() + chrono::Duration::seconds(sec.saturating_sub(60)));
        }
        Ok(true)
    }

    async fn fetch_quota(
        &self,
        account: &Account,
        client: &Client,
    ) -> Result<AccountQuota, ProviderError> {
        let mut credential = account.credential.clone();
        let _ = self.refresh_token(&mut credential, client).await;
        let token = credential
            .token_str()
            .ok_or_else(|| ProviderError::Authentication("Missing Grok access token".into()))?;

        match fetch_billing(client, token).await {
            Ok((windows, plan)) => Ok(AccountQuota::success(
                &account.id,
                ProviderId::Grok,
                &account.label,
                plan.or_else(|| Some("Grok".into())),
                None,
                windows,
            )),
            Err(ProviderError::TokenExpired(_)) => {
                let mut cred = credential.clone();
                self.refresh_token(&mut cred, client).await?;
                let token = cred.token_str().ok_or_else(|| {
                    ProviderError::Authentication("Grok refresh produced an empty token".into())
                })?;
                let (windows, plan) = fetch_billing(client, token).await?;
                Ok(AccountQuota::success(
                    &account.id,
                    ProviderId::Grok,
                    &account.label,
                    plan.or_else(|| Some("Grok".into())),
                    None,
                    windows,
                ))
            }
            Err(err) => Ok(AccountQuota::failed(
                &account.id,
                ProviderId::Grok,
                &account.label,
                err.to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_weekly_billing() {
        let root = json!({
            "config": {
                "creditUsagePercent": 55.0,
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "end": "2026-09-22T01:20:14.125878+00:00"
                },
                "onDemandCap": { "val": 0 },
                "productUsage": [
                    { "product": "GrokBuild", "usagePercent": 55.0 },
                    { "product": "GrokChat" }
                ]
            }
        });
        let windows = parse_grok_billing(&root);
        assert_eq!(windows[0].name, "Grok Weekly");
        assert!((windows[0].remaining_percent - 45.0).abs() < 0.01);
        assert_eq!(windows[0].period, QuotaPeriod::Weekly7D);
        assert!(windows.iter().any(|w| w.name == "Grok Build"));
        assert!(!windows.iter().any(|w| w.name == "Grok Extra"));
    }
}
