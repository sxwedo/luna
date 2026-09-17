use super::{Provider, ProviderError};
use crate::domain::account::{Account, ProviderId};
use crate::domain::credential::AuthCredential;
use crate::domain::quota::{AccountQuota, QuotaPeriod, QuotaWindow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;
use tokio::task::JoinSet;

/// Region roots tried in order: 中国大陆 (bigmodel.cn) first, then global z.ai.
const DEFAULT_ROOTS: &[&str] = &["https://open.bigmodel.cn", "https://api.z.ai"];

#[derive(Default, Clone)]
pub struct ZhipuProvider;

fn json_number(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))
        .or_else(|| value.as_u64().map(|n| n as f64))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn window_from_unit(unit: f64, number: f64) -> (QuotaPeriod, String) {
    match (unit.round() as i64, number) {
        (3, n) if n > 0.0 && n < 24.0 => (QuotaPeriod::Session5H, "GLM 5H".into()),
        (3, _) => (QuotaPeriod::Daily, "GLM Daily".into()),
        (4, n) if n <= 1.0 => (QuotaPeriod::Daily, "GLM Daily".into()),
        (4, n) if n < 28.0 => (QuotaPeriod::Weekly7D, "GLM Weekly".into()),
        (5, _) => (QuotaPeriod::Monthly, "GLM Monthly".into()),
        (6, n) if n < 4.0 => (QuotaPeriod::Weekly7D, "GLM Weekly".into()),
        (6, _) => (QuotaPeriod::Monthly, "GLM Monthly".into()),
        _ => (QuotaPeriod::Custom, "GLM Quota".into()),
    }
}

/// Parse Zhipu / GLM quota JSON (`/api/monitor/usage/quota/limit`).
/// `percentage` is used%; we convert to remaining.
pub fn parse_zhipu_quota(root: &Value) -> Vec<QuotaWindow> {
    if root.get("success").and_then(Value::as_bool) == Some(false) {
        return Vec::new();
    }
    if root
        .get("code")
        .and_then(Value::as_i64)
        .is_some_and(|code| code != 200)
    {
        return Vec::new();
    }
    let Some(limits) = root.pointer("/data/limits").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut windows = Vec::new();
    for slot in limits {
        let kind = slot
            .get("type")
            .or_else(|| slot.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !matches!(kind, "TOKENS_LIMIT" | "CREDIT_LIMIT" | "TIME_LIMIT") {
            continue;
        }

        let unit = json_number(slot.get("unit")).unwrap_or(0.0);
        let number = json_number(slot.get("number")).unwrap_or(0.0);
        let (period, name) = if kind == "TIME_LIMIT" {
            (QuotaPeriod::Custom, "GLM Searches".to_string())
        } else {
            window_from_unit(unit, number)
        };

        let remaining = if kind == "TIME_LIMIT" {
            let used = json_number(slot.get("currentValue")).unwrap_or(0.0);
            let limit = json_number(slot.get("usage")).unwrap_or(0.0);
            if limit > 0.0 {
                ((limit - used) / limit * 100.0).clamp(0.0, 100.0)
            } else {
                0.0
            }
        } else {
            let used_pct = json_number(slot.get("percentage")).unwrap_or(0.0);
            (100.0 - used_pct).clamp(0.0, 100.0)
        };

        let resets_at = json_number(slot.get("nextResetTime")).and_then(|ms| {
            DateTime::from_timestamp_millis(ms.round() as i64).map(|dt| dt.with_timezone(&Utc))
        });

        windows.push(QuotaWindow::new(name, period, remaining, resets_at));
    }
    windows
}

fn parse_plan(root: &Value) -> Option<String> {
    root.pointer("/data/0/productName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[async_trait]
impl Provider for ZhipuProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Zhipu
    }

    fn name(&self) -> &'static str {
        "智谱 GLM"
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
                    "Missing Zhipu API key".into(),
                ))
            }
        };

        let roots: Vec<String> = match &account.credential {
            AuthCredential::ApiKey {
                base_url: Some(url),
                ..
            } if !url.is_empty() => vec![url.trim_end_matches('/').to_string()],
            _ => DEFAULT_ROOTS.iter().map(|r| r.to_string()).collect(),
        };

        // Race every region root concurrently: the first one that returns usable
        // quota data wins and the stragglers are aborted. A mainland-only key on
        // z.ai (or vice versa) fails fast instead of eating the whole timeout.
        let mut set: JoinSet<Result<(Vec<QuotaWindow>, Option<String>), String>> = JoinSet::new();
        for root in roots {
            let client = client.clone();
            let token = token.to_string();
            set.spawn(async move {
                let quota_url = format!("{root}/api/monitor/usage/quota/limit");
                let resp = client
                    .get(&quota_url)
                    .bearer_auth(&token)
                    .header("Accept", "application/json")
                    .timeout(Duration::from_secs(6))
                    .send()
                    .await
                    .map_err(|e| {
                        if e.is_timeout() {
                            format!("{root}: 响应超时 (该区域服务端响应缓慢或不可达)")
                        } else {
                            format!("{root}: 网络错误 {e}")
                        }
                    })?;
                if !resp.status().is_success() {
                    return Err(format!("{root}: HTTP {}", resp.status()));
                }
                let body: Value = resp
                    .json()
                    .await
                    .map_err(|_| format!("{root}: 响应解析失败"))?;
                let windows = parse_zhipu_quota(&body);
                if windows.is_empty() {
                    let msg = body
                        .get("msg")
                        .and_then(Value::as_str)
                        .unwrap_or("未返回配额数据");
                    let hint = if msg.contains("Internal service error")
                        || msg.contains("内部服务器错误")
                    {
                        " (服务端拒绝处理该 Key，可能区域不匹配或无配额查询权限)"
                    } else if msg.contains("token") || msg.contains("incorrect") {
                        " (API Key 无效或已过期)"
                    } else {
                        ""
                    };
                    return Err(format!("{root}: {msg}{hint}"));
                }

                let sub_url = format!("{root}/api/biz/subscription/list");
                let plan = match client
                    .get(&sub_url)
                    .bearer_auth(token)
                    .header("Accept", "application/json")
                    .timeout(Duration::from_secs(5))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        resp.json::<Value>().await.ok().and_then(|v| parse_plan(&v))
                    }
                    _ => None,
                };
                Ok((windows, plan))
            });
        }

        let mut errors: Vec<String> = Vec::new();
        while let Some(res) = set.join_next().await {
            match res {
                Ok(Ok((windows, plan))) => {
                    return Ok(AccountQuota::success(
                        &account.id,
                        ProviderId::Zhipu,
                        &account.label,
                        plan.or_else(|| Some("智谱 GLM".into())),
                        None,
                        windows,
                    ));
                }
                Ok(Err(msg)) => errors.push(msg),
                Err(e) => errors.push(format!("任务失败: {e}")),
            }
        }

        Ok(AccountQuota::failed(
            &account.id,
            ProviderId::Zhipu,
            &account.label,
            format!(
                "{} (确认 API Key 所属区域: bigmodel.cn 中国大陆 / z.ai 海外)",
                errors.join("; ")
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::quota::QuotaPeriod;

    #[test]
    fn test_parse_zhipu_quota_tokens_and_session() {
        let raw = serde_json::json!({
            "code": 200,
            "success": true,
            "data": {
                "limits": [
                    {
                        "type": "TOKENS_LIMIT",
                        "unit": 3,
                        "number": 5,
                        "percentage": 42.5,
                        "nextResetTime": 1774000000000.0
                    },
                    {
                        "type": "TOKENS_LIMIT",
                        "unit": 6,
                        "number": 1,
                        "percentage": 10,
                        "nextResetTime": 1774600000000.0
                    }
                ]
            }
        });
        let windows = parse_zhipu_quota(&raw);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].period, QuotaPeriod::Session5H);
        assert!((windows[0].remaining_percent - 57.5).abs() < 0.01);
        assert_eq!(windows[1].period, QuotaPeriod::Weekly7D);
        assert!((windows[1].remaining_percent - 90.0).abs() < 0.01);
    }
}
