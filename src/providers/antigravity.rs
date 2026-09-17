use super::{Provider, ProviderError};
use crate::domain::account::{Account, ProviderId};
use crate::domain::credential::AuthCredential;
use crate::domain::quota::{AccountQuota, QuotaPeriod, QuotaWindow};
use crate::storage::keychain::KeychainReader;
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use rand::RngCore;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinSet;

pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo?alt=json";
pub const USER_AGENT: &str =
    "antigravity/cli/1.1.23 (aidev_client; os_type=linux; arch=amd64; cl=974125021; auth_method=consumer)";

/// Same order as pi-antigravity: daily first (live 5H), then sandbox, then prod.
pub const API_ENDPOINTS: &[&str] = &[
    "https://daily-cloudcode-pa.googleapis.com/v1internal:",
    "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal:",
    "https://cloudcode-pa.googleapis.com/v1internal:",
];

#[derive(Default, Clone)]
pub struct AntigravityProvider;

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UserInfoResponse {
    email: Option<String>,
}

impl AntigravityProvider {
    pub fn new() -> Self {
        Self
    }

    pub async fn fetch_user_email(client: &Client, access_token: &str) -> Option<String> {
        let resp = client
            .get(USERINFO_URL)
            .bearer_auth(access_token)
            .send()
            .await
            .ok()?;
        resp.json::<UserInfoResponse>().await.ok()?.email
    }

    /// Import credentials from the official Antigravity IDE (macOS Keychain) and resolve the email.
    pub async fn sniff_installed_credentials(
        client: &Client,
    ) -> Result<Option<Account>, ProviderError> {
        let token = match KeychainReader::read_antigravity_token()
            .map_err(|e| ProviderError::Authentication(e.to_string()))?
        {
            Some(token) if token.access_token.is_some() => token,
            _ => return Ok(None),
        };

        let expires_at = token
            .expiry
            .and_then(|exp_str| DateTime::parse_from_rfc3339(&exp_str).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let (client_id, client_secret) = oauth_client_pair()?;
        let mut cred = AuthCredential::OAuth {
            access_token: token.access_token.unwrap_or_default(),
            refresh_token: token.refresh_token,
            expires_at,
            client_id: Some(client_id),
            client_secret: Some(client_secret),
            project_id: None,
        };

        let provider = AntigravityProvider;
        let _ = provider.refresh_token(&mut cred, client).await;

        let access_token = cred
            .token_str()
            .ok_or_else(|| ProviderError::Authentication("Keychain token is empty".into()))?
            .to_string();

        let email = Self::fetch_user_email(client, &access_token).await;
        let label = email.clone().ok_or_else(|| {
            ProviderError::Authentication("Could not resolve account email from IDE token".into())
        })?;
        let id = Account::identity_id(ProviderId::Antigravity, &label);

        Ok(Some(Account::new(id, ProviderId::Antigravity, label, cred)))
    }

    /// Perform OAuth 2.0 PKCE login via browser.
    pub async fn login_interactive() -> Result<Account, ProviderError> {
        let (client_id, client_secret) = oauth_client_pair()?;

        // 1. Generate PKCE verifier and challenge
        let mut verifier_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut verifier_bytes);
        let code_verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

        let mut hasher = Sha256::new();
        hasher.update(code_verifier.as_bytes());
        let challenge_hash = hasher.finalize();
        let code_challenge = URL_SAFE_NO_PAD.encode(challenge_hash);

        let mut state_bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut state_bytes);
        let state = URL_SAFE_NO_PAD.encode(state_bytes);

        // 2. Bind local HTTP server for callback
        let port = 51121;
        let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();
        let listener = TcpListener::bind(addr).await.map_err(|e| {
            ProviderError::Other(format!(
                "Failed to bind local port {} for OAuth callback: {}",
                port, e
            ))
        })?;

        let redirect_uri = format!("http://localhost:{}/oauth-callback", port);

        let auth_url = format!(
            "{}?client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&access_type=offline&prompt=consent",
            AUTH_URL,
            urlencoding(&client_id),
            urlencoding(&redirect_uri),
            urlencoding("https://www.googleapis.com/auth/aicode https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile"),
            code_challenge,
            state
        );

        println!("\n  === Google Antigravity OAuth Login ===");
        println!("  Please open the following authorization URL in your browser:\n");
        println!("  \x1b[36m{}\x1b[0m\n", auth_url);
        println!(
            "  Waiting for browser authorization on http://localhost:{}/oauth-callback ...",
            port
        );

        // Wait for incoming connection with 3 minutes timeout
        let auth_code = tokio::select! {
            res = wait_for_oauth_code(listener, &state) => res?,
            _ = tokio::time::sleep(Duration::from_secs(180)) => {
                return Err(ProviderError::Other("OAuth login timed out after 3 minutes".to_string()));
            }
        };

        // 3. Exchange code for tokens
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(ProviderError::Network)?;

        let token_resp: TokenResponse = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&[
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("code", auth_code.as_str()),
                ("grant_type", "authorization_code"),
                ("redirect_uri", redirect_uri.as_str()),
                ("code_verifier", code_verifier.as_str()),
            ])
            .send()
            .await?
            .json()
            .await?;

        let expires_at = token_resp
            .expires_in
            .map(|sec| Utc::now() + chrono::Duration::seconds(sec - 300));

        // 4. Fetch user email
        let email = match client
            .get(USERINFO_URL)
            .bearer_auth(&token_resp.access_token)
            .send()
            .await
        {
            Ok(resp) => resp
                .json::<UserInfoResponse>()
                .await
                .ok()
                .and_then(|u| u.email),
            Err(_) => None,
        };

        let label = email
            .clone()
            .unwrap_or_else(|| "antigravity-user".to_string());
        let id = format!("antigravity:{}", email.as_deref().unwrap_or("default"));

        let cred = AuthCredential::OAuth {
            access_token: token_resp.access_token,
            refresh_token: token_resp.refresh_token,
            expires_at,
            client_id: Some(client_id),
            client_secret: Some(client_secret),
            project_id: None,
        };

        Ok(Account::new(id, ProviderId::Antigravity, label, cred))
    }
}

fn oauth_client_pair() -> Result<(String, String), ProviderError> {
    crate::storage::config::antigravity_oauth()
        .map_err(|e| ProviderError::Authentication(e.to_string()))
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
            .map_err(|e| ProviderError::Other(format!("Socket accept error: {}", e)))?;

        let mut buf = [0u8; 2048];
        let n = socket
            .read(&mut buf)
            .await
            .map_err(|e| ProviderError::Other(e.to_string()))?;
        let request = String::from_utf8_lossy(&buf[..n]);

        // Parse GET line
        if let Some(first_line) = request.lines().next() {
            if let Some(path) = first_line.split_whitespace().nth(1) {
                if path.starts_with("/oauth-callback") {
                    let full_url = format!("http://localhost{}", path);
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
                                let body = "<html><body><h2>Antigravity Login Successful!</h2><p>You may now close this window and return to your terminal.</p></body></html>";
                                let response = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    body.len(),
                                    body
                                );
                                let _ = socket.write_all(response.as_bytes()).await;
                                return Ok(code);
                            }
                        }
                    }
                }
            }
        }

        let body = "Invalid or missing OAuth state/code";
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes()).await;
    }
}

#[async_trait]
impl Provider for AntigravityProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Antigravity
    }

    fn name(&self) -> &'static str {
        "Google Antigravity"
    }

    async fn refresh_token(
        &self,
        credential: &mut AuthCredential,
        client: &Client,
    ) -> Result<bool, ProviderError> {
        if !credential.is_expired(300) {
            return Ok(false);
        }

        let (refresh_token, stored_id, stored_secret) = match credential {
            AuthCredential::OAuth {
                refresh_token: Some(ref rt),
                client_id,
                client_secret,
                ..
            } => (rt.clone(), client_id.clone(), client_secret.clone()),
            _ => return Ok(false),
        };

        let (client_id, client_secret) = match (
            stored_id.filter(|s| !s.is_empty()),
            stored_secret.filter(|s| !s.is_empty()),
        ) {
            (Some(id), Some(secret)) => (id, secret),
            _ => oauth_client_pair()?,
        };

        let resp = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&[
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("refresh_token", refresh_token.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TokenExpired(format!(
                "Failed to refresh Google OAuth token: {}",
                body
            )));
        }

        let token_resp: TokenResponse = resp.json().await?;
        let new_expires_at = token_resp
            .expires_in
            .map(|sec| Utc::now() + chrono::Duration::seconds(sec - 300));

        if let AuthCredential::OAuth {
            access_token,
            expires_at,
            ..
        } = credential
        {
            *access_token = token_resp.access_token;
            *expires_at = new_expires_at;
        }

        Ok(true)
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
                    "No access token found".into(),
                ))
            }
        };

        // Plan lookup and quota summaries run concurrently: one round-trip total,
        // regardless of how many backend endpoints we probe.
        let (assist, summaries) = tokio::join!(
            load_code_assist(client, token),
            fetch_quota_summaries(client, token)
        );
        let (plan, project_id) = assist.unwrap_or((None, None));

        let mut windows = merge_quota_summaries(&summaries);

        // Fallback: retrieveUserQuotaSummary denied (e.g. free tier 403) →
        // fetchAvailableModels still exposes per-model quotaInfo.
        if windows.is_empty() {
            windows = fetch_available_models(client, token)
                .await
                .map(|val| parse_models_quota_windows(&val))
                .unwrap_or_default();
        }

        if windows.is_empty() {
            return Ok(AccountQuota::failed(
                &account.id,
                ProviderId::Antigravity,
                &account.label,
                "No quota metrics returned. Account may be unauthenticated or lacking Gemini license.",
            ));
        }

        // Apply monotonic clamp filter: suppresses read-replication jitter (e.g. 1.4% <-> 1.9%)
        // across Google's distributed load-balanced endpoints within the same reset window.
        static MONOTONIC_CACHE: LazyLock<
            Mutex<HashMap<String, HashMap<String, (Option<DateTime<Utc>>, f64)>>>,
        > = LazyLock::new(|| Mutex::new(HashMap::new()));

        if let Ok(mut lock) = MONOTONIC_CACHE.lock() {
            let account_map = lock.entry(account.id.clone()).or_default();
            for window in &mut windows {
                if let Some((prev_reset, prev_pct)) = account_map.get(&window.name) {
                    if window.resets_at == *prev_reset {
                        // In the same session cycle, a small upward glitch (< 5%) is an out-of-sync read replica.
                        // Force monotonic decrease to guarantee absolute stability.
                        if window.remaining_percent > *prev_pct
                            && window.remaining_percent - *prev_pct < 5.0
                        {
                            window.remaining_percent = *prev_pct;
                        }
                    }
                }
                account_map.insert(
                    window.name.clone(),
                    (window.resets_at, window.remaining_percent),
                );
            }
        }

        Ok(AccountQuota::success(
            &account.id,
            ProviderId::Antigravity,
            &account.label,
            plan,
            project_id,
            windows,
        ))
    }
}

#[derive(Clone)]
struct RawWindow {
    group_label: String,
    bucket_id: String,
    period: QuotaPeriod,
    remaining: f64,
    resets_at: Option<DateTime<Utc>>,
    description: Option<String>,
    priority: usize,
}

/// POST loadCodeAssist against every endpoint concurrently; first usable
/// response wins (paidTier preferred over currentTier).
async fn load_code_assist(
    client: &Client,
    token: &str,
) -> Option<(Option<String>, Option<String>)> {
    let mut set = JoinSet::new();
    for endpoint in API_ENDPOINTS {
        let client = client.clone();
        let token = token.to_string();
        set.spawn(async move {
            let resp = client
                .post(format!("{endpoint}loadCodeAssist"))
                .bearer_auth(token)
                .header("User-Agent", USER_AGENT)
                .timeout(Duration::from_secs(6))
                .json(&json!({
                    "metadata": {
                        "ideType": "ANTIGRAVITY",
                        "platform": "PLATFORM_UNSPECIFIED",
                        "pluginType": "GEMINI"
                    }
                }))
                .send()
                .await
                .ok()?;
            if !resp.status().is_success() {
                return None;
            }
            resp.json::<Value>().await.ok()
        });
    }
    while let Some(res) = set.join_next().await {
        let Some(val) = res.ok().flatten() else {
            continue;
        };
        let paid = val.pointer("/paidTier/name").and_then(Value::as_str);
        let curr = val.pointer("/currentTier/name").and_then(Value::as_str);
        let plan = paid.or(curr).map(str::to_string);
        let project = val
            .pointer("/cloudaicompanionProject")
            .and_then(Value::as_str)
            .map(str::to_string);
        return Some((plan, project));
    }
    None
}

/// POST retrieveUserQuotaSummary at every endpoint concurrently.
/// cloudcode-pa often stubs the 5H bucket at 1.0 with no description, while
/// daily-cloudcode-pa carries the live window — so we keep all successful
/// responses and let `merge_quota_summaries` pick the live one.
/// Results are re-ordered by endpoint preference so merge tie-breaks stay deterministic.
async fn fetch_quota_summaries(client: &Client, token: &str) -> Vec<Value> {
    let mut set: JoinSet<(usize, Option<Value>)> = JoinSet::new();
    for (idx, endpoint) in API_ENDPOINTS.iter().enumerate() {
        let client = client.clone();
        let token = token.to_string();
        let endpoint = endpoint.to_string();
        set.spawn(async move {
            let resp = client
                .post(format!("{endpoint}retrieveUserQuotaSummary"))
                .bearer_auth(token)
                .header("User-Agent", USER_AGENT)
                .timeout(Duration::from_secs(8))
                .json(&json!({}))
                .send()
                .await
                .ok();
            let val = match resp {
                Some(resp) if resp.status().is_success() => {
                    if let Ok(mut v) = resp.json::<Value>().await {
                        if v.get("groups").and_then(Value::as_array).is_some() {
                            if let Some(obj) = v.as_object_mut() {
                                obj.insert("__priority".to_string(), json!(idx));
                            }
                            Some(v)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                _ => None,
            };
            (idx, val)
        });
    }
    let mut indexed: Vec<(usize, Value)> = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok((idx, Some(val))) = res {
            indexed.push((idx, val));
        }
    }
    indexed.sort_by_key(|(idx, _)| *idx);
    indexed.into_iter().map(|(_, val)| val).collect()
}

/// POST fetchAvailableModels at every endpoint concurrently; first non-empty
/// model catalog wins.
async fn fetch_available_models(client: &Client, token: &str) -> Option<Value> {
    let mut set = JoinSet::new();
    for endpoint in API_ENDPOINTS {
        let client = client.clone();
        let token = token.to_string();
        set.spawn(async move {
            let resp = client
                .post(format!("{endpoint}fetchAvailableModels"))
                .bearer_auth(token)
                .header("User-Agent", USER_AGENT)
                .timeout(Duration::from_secs(6))
                .json(&json!({}))
                .send()
                .await
                .ok()?;
            if !resp.status().is_success() {
                return None;
            }
            resp.json::<Value>().await.ok()
        });
    }
    while let Some(res) = set.join_next().await {
        if let Some(val) = res.ok().flatten() {
            if val.get("models").and_then(Value::as_object).is_some() {
                return Some(val);
            }
        }
    }
    None
}

fn json_number(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))
        .or_else(|| value.as_u64().map(|n| n as f64))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn is_stub_window(remaining: f64, description: Option<&str>) -> bool {
    remaining >= 0.999 && description.map(str::is_empty).unwrap_or(true)
}

fn collect_raw_windows(val: &Value) -> Vec<RawWindow> {
    let mut windows = Vec::new();
    let groups = match val.get("groups").and_then(Value::as_array) {
        Some(g) => g,
        None => return windows,
    };

    for group in groups {
        let group_name = group
            .get("displayName")
            .or_else(|| group.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();

        let group_label = if group_name.contains("gemini") {
            "Gemini"
        } else if group_name.contains("claude")
            || group_name.contains("gpt")
            || group_name.contains("3p")
        {
            "Claude/GPT"
        } else {
            "General"
        };

        let Some(buckets) = group.get("buckets").and_then(Value::as_array) else {
            continue;
        };
        for bucket in buckets {
            let bucket_id = bucket
                .get("bucketId")
                .or_else(|| bucket.get("id"))
                .or_else(|| bucket.get("displayName"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            let window_tag = bucket
                .get("window")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            let combined_tag = format!("{bucket_id} {window_tag}");

            let period = if combined_tag.contains("week")
                || combined_tag.contains("7d")
                || combined_tag.contains("seven")
            {
                QuotaPeriod::Weekly7D
            } else if combined_tag.contains("session")
                || combined_tag.contains("5h")
                || combined_tag.contains("five")
                || combined_tag.contains("hour")
            {
                QuotaPeriod::Session5H
            } else {
                QuotaPeriod::Custom
            };

            let remaining = json_number(
                bucket
                    .get("remainingFraction")
                    .or_else(|| bucket.get("remaining_fraction")),
            )
            .unwrap_or(0.0);

            let resets_at = bucket
                .get("resetTime")
                .or_else(|| bucket.get("reset_time"))
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));

            let description = bucket
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);

            windows.push(RawWindow {
                group_label: group_label.to_string(),
                bucket_id,
                period,
                remaining,
                resets_at,
                description,
                priority: val.get("__priority").and_then(Value::as_u64).unwrap_or(99) as usize,
            });
        }
    }
    windows
}

fn raw_to_quota_window(raw: RawWindow) -> QuotaWindow {
    let window_name = match raw.period {
        QuotaPeriod::Session5H => format!("{} 5H", raw.group_label),
        QuotaPeriod::Weekly7D => format!("{} Weekly", raw.group_label),
        _ => format!("{} {}", raw.group_label, raw.bucket_id),
    };
    let mut w = QuotaWindow::new(
        window_name,
        raw.period,
        raw.remaining * 100.0,
        raw.resets_at,
    );
    w.note = Some(format!("Bucket: {}", raw.bucket_id));
    w
}

fn sort_windows(windows: &mut [QuotaWindow]) {
    windows.sort_by_key(|w| match (w.name.contains("Gemini"), w.period) {
        (true, QuotaPeriod::Session5H) => 1,
        (true, QuotaPeriod::Weekly7D) => 2,
        (false, QuotaPeriod::Session5H) => 3,
        (false, QuotaPeriod::Weekly7D) => 4,
        _ => 5,
    });
}

/// Merge summaries from multiple Google backends.
/// Live 5H data lives on daily-cloudcode-pa; cloudcode-pa often stubs 5H at 1.0.
pub fn merge_quota_summaries(summaries: &[Value]) -> Vec<QuotaWindow> {
    let mut best: BTreeMap<(String, QuotaPeriod), RawWindow> = BTreeMap::new();
    for val in summaries {
        for raw in collect_raw_windows(val) {
            let key = (raw.group_label.clone(), raw.period);
            match best.get(&key) {
                None => {
                    best.insert(key, raw);
                }
                Some(existing) => {
                    let new_live = !is_stub_window(raw.remaining, raw.description.as_deref());
                    let old_live =
                        !is_stub_window(existing.remaining, existing.description.as_deref());
                    let take = match (new_live, old_live) {
                        (true, false) => true,
                        (false, true) => false,
                        // If both are live or both are stubs, strictly obey endpoint priority!
                        // This prevents thrashing between daily (1.4%) and sandbox (1.9%).
                        _ => raw.priority < existing.priority,
                    };
                    if take {
                        best.insert(key, raw);
                    }
                }
            }
        }
    }
    let mut windows: Vec<QuotaWindow> = best.into_values().map(raw_to_quota_window).collect();
    sort_windows(&mut windows);
    windows
}

/// Parses a single quota summary response into Session & Weekly windows.
pub fn parse_quota_summary_windows(val: &Value) -> Vec<QuotaWindow> {
    merge_quota_summaries(std::slice::from_ref(val))
}

/// Fallback parser when only fetchAvailableModels is accessible.
pub fn parse_models_quota_windows(val: &Value) -> Vec<QuotaWindow> {
    let mut windows = Vec::new();
    let models = match val.get("models").and_then(Value::as_object) {
        Some(m) => m,
        None => return windows,
    };

    for (model_id, model_info) in models {
        if model_id.starts_with("chat_") || model_id.starts_with("tab_") {
            continue;
        }

        if let Some(quota_info) = model_info.get("quotaInfo") {
            let remaining_fraction = quota_info
                .get("remainingFraction")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);

            let remaining_percent = remaining_fraction * 100.0;

            let reset_time = quota_info
                .get("resetTime")
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));

            let display_name = model_info
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or(model_id);

            let mut w = QuotaWindow::new(
                display_name,
                QuotaPeriod::PoolShared,
                remaining_percent,
                reset_time,
            );
            w.note = Some("Shared Pool".to_string());
            windows.push(w);
        }
    }

    windows.sort_by(|a, b| a.name.cmp(&b.name));
    windows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_quota_summary_windows_all_windows() {
        let raw = serde_json::json!({
            "groups": [
                {
                    "displayName": "Gemini",
                    "buckets": [
                        {
                            "bucketId": "gemini-session",
                            "displayName": "Session Quota",
                            "window": "5h",
                            "remainingFraction": 0.825,
                            "resetTime": "2026-03-24T18:30:00Z"
                        },
                        {
                            "bucketId": "gemini-weekly",
                            "displayName": "Weekly Quota",
                            "window": "7d",
                            "remainingFraction": 0.65,
                            "resetTime": "2026-03-29T12:00:00Z"
                        }
                    ]
                },
                {
                    "displayName": "Claude and GPT",
                    "buckets": [
                        {
                            "bucketId": "3p-session",
                            "displayName": "Session Quota",
                            "window": "session",
                            "remainingFraction": 0.40,
                            "resetTime": "2026-03-24T17:15:00Z"
                        },
                        {
                            "bucketId": "3p-weekly",
                            "displayName": "Weekly Quota",
                            "window": "weekly",
                            "remainingFraction": 0.90,
                            "resetTime": "2026-03-30T00:00:00Z"
                        }
                    ]
                }
            ]
        });

        let windows = parse_quota_summary_windows(&raw);
        assert_eq!(windows.len(), 4);

        assert_eq!(windows[0].name, "Gemini 5H");
        assert_eq!(windows[0].period, QuotaPeriod::Session5H);
        assert!((windows[0].remaining_percent - 82.5).abs() < 0.001);

        assert_eq!(windows[1].name, "Gemini Weekly");
        assert_eq!(windows[1].period, QuotaPeriod::Weekly7D);
        assert!((windows[1].remaining_percent - 65.0).abs() < 0.001);

        assert_eq!(windows[2].name, "Claude/GPT 5H");
        assert_eq!(windows[2].period, QuotaPeriod::Session5H);
        assert!((windows[2].remaining_percent - 40.0).abs() < 0.001);

        assert_eq!(windows[3].name, "Claude/GPT Weekly");
        assert_eq!(windows[3].period, QuotaPeriod::Weekly7D);
        assert!((windows[3].remaining_percent - 90.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_models_quota_windows_fallback() {
        let raw = serde_json::json!({
            "models": {
                "gemini-3.1-pro-high": {
                    "displayName": "Gemini 3.1 Pro (High)",
                    "quotaInfo": {
                        "remainingFraction": 0.75,
                        "resetTime": "2026-03-24T18:00:00Z"
                    }
                },
                "claude-sonnet-4-6": {
                    "displayName": "Claude 4.6 Sonnet",
                    "quotaInfo": {
                        "remainingFraction": 0.30,
                        "resetTime": "2026-03-24T19:00:00Z"
                    }
                },
                "chat_internal": {
                    "quotaInfo": { "remainingFraction": 1.0 }
                }
            }
        });

        let windows = parse_models_quota_windows(&raw);
        assert_eq!(windows.len(), 2);
        assert!(windows.iter().any(
            |w| w.name == "Gemini 3.1 Pro (High)" && (w.remaining_percent - 75.0).abs() < 0.001
        ));
        assert!(windows
            .iter()
            .any(|w| w.name == "Claude 4.6 Sonnet" && (w.remaining_percent - 30.0).abs() < 0.001));
    }

    #[test]
    fn test_merge_prefers_live_5h_over_prod_stub() {
        let prod_stub = serde_json::json!({
            "groups": [{
                "displayName": "Gemini Models",
                "buckets": [
                    {"bucketId": "gemini-weekly", "window": "weekly", "remainingFraction": 0.31, "description": "used weekly"},
                    {"bucketId": "gemini-5h", "window": "5h", "remainingFraction": 1}
                ]
            }]
        });
        let daily_live = serde_json::json!({
            "groups": [{
                "displayName": "Gemini Models",
                "buckets": [
                    {"bucketId": "gemini-weekly", "window": "weekly", "remainingFraction": 0.85, "description": "used weekly"},
                    {"bucketId": "gemini-5h", "window": "5h", "remainingFraction": 0.173, "description": "used 5-hour"}
                ]
            }]
        });

        let windows = merge_quota_summaries(&[prod_stub, daily_live]);
        let five_h = windows
            .iter()
            .find(|w| w.period == QuotaPeriod::Session5H)
            .unwrap();
        let weekly = windows
            .iter()
            .find(|w| w.period == QuotaPeriod::Weekly7D)
            .unwrap();
        assert!((five_h.remaining_percent - 17.3).abs() < 0.05);
        assert!((weekly.remaining_percent - 31.0).abs() < 0.05);
    }
}
