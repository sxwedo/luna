use super::account::ProviderId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Type of quota replenishment or rolling duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaPeriod {
    /// 5-Hour short-term session window (e.g. Antigravity gemini-session / 3p-session).
    Session5H,
    /// 7-Day rolling weekly window (e.g. Antigravity gemini-weekly / 3p-weekly).
    Weekly7D,
    /// Daily window.
    Daily,
    /// Monthly window.
    Monthly,
    /// Model or token-pool level quota.
    PoolShared,
    /// Unspecified / Custom.
    Custom,
}

impl QuotaPeriod {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Session5H => "5-Hour Session",
            Self::Weekly7D => "7-Day Weekly",
            Self::Daily => "Daily",
            Self::Monthly => "Monthly",
            Self::PoolShared => "Pool Shared",
            Self::Custom => "Custom Window",
        }
    }
}

/// A specific quota window metric.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaWindow {
    /// Window or metric identifier/name (e.g. "Gemini Session (5H)").
    pub name: String,
    /// The window replenishment period.
    pub period: QuotaPeriod,
    /// Remaining percentage (0.0% to 100.0%).
    pub remaining_percent: f64,
    /// Absolute reset timestamp when quota replenishes.
    pub resets_at: Option<DateTime<Utc>>,
    /// Contextual note (e.g. "Shared across Gemini models").
    pub note: Option<String>,
}

impl QuotaWindow {
    pub fn new(
        name: impl Into<String>,
        period: QuotaPeriod,
        remaining_percent: f64,
        resets_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            name: name.into(),
            period,
            remaining_percent: remaining_percent.clamp(0.0, 100.0),
            resets_at,
            note: None,
        }
    }

    /// Formats the remaining time until reset into a concise human string (e.g. "3h 25m", "5d 12h").
    pub fn format_reset_time(&self) -> String {
        match self.resets_at {
            Some(resets_at) => {
                let now = Utc::now();
                if resets_at <= now {
                    return "now (ready)".to_string();
                }
                let duration = resets_at.signed_duration_since(now);
                let total_minutes = duration.num_minutes();
                let days = total_minutes / (60 * 24);
                let hours = (total_minutes % (60 * 24)) / 60;
                let minutes = total_minutes % 60;

                if days > 0 {
                    format!("{}d {}h", days, hours)
                } else if hours > 0 {
                    format!("{}h {}m", hours, minutes)
                } else {
                    format!("{}m", minutes.max(1))
                }
            }
            None => "n/a".to_string(),
        }
    }
}

/// Complete quota snapshot for an account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountQuota {
    pub account_id: String,
    pub provider: ProviderId,
    pub label: String,
    /// Subscription plan name (e.g. "Google AI Pro", "ChatGPT Plus").
    pub plan: Option<String>,
    /// Project identifier (e.g. Google Cloud Companion Project ID).
    pub project_id: Option<String>,
    /// All individual quota windows.
    pub windows: Vec<QuotaWindow>,
    /// Error message if quota fetch failed.
    pub error: Option<String>,
    /// Snapshot timestamp.
    pub fetched_at: DateTime<Utc>,
}

impl AccountQuota {
    pub fn success(
        account_id: impl Into<String>,
        provider: ProviderId,
        label: impl Into<String>,
        plan: Option<String>,
        project_id: Option<String>,
        windows: Vec<QuotaWindow>,
    ) -> Self {
        Self {
            account_id: account_id.into(),
            provider,
            label: label.into(),
            plan,
            project_id,
            windows,
            error: None,
            fetched_at: Utc::now(),
        }
    }

    pub fn failed(
        account_id: impl Into<String>,
        provider: ProviderId,
        label: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            account_id: account_id.into(),
            provider,
            label: label.into(),
            plan: None,
            project_id: None,
            windows: Vec::new(),
            error: Some(error.into()),
            fetched_at: Utc::now(),
        }
    }

    /// Returns the lowest remaining quota window (the bottleneck/limiter).
    pub fn bottleneck(&self) -> Option<&QuotaWindow> {
        self.windows
            .iter()
            .min_by(|a, b| a.remaining_percent.total_cmp(&b.remaining_percent))
    }
}

/// Detailed quota difference for a single window between consecutive refreshes.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowDiff {
    pub name: String,
    pub previous_percent: f64,
    pub current_percent: f64,
    /// current - previous (e.g. -2.5 means 2.5% consumed, +100.0 means replenished)
    pub delta: f64,
}

/// Diff summary for an account between consecutive refreshes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountDiff {
    pub account_id: String,
    /// True if any window has noticeable consumption (delta <= -0.05%)
    pub is_consuming: bool,
    /// True if any window was replenished/reset (delta >= +5.0%)
    pub is_replenished: bool,
    pub window_diffs: Vec<WindowDiff>,
}

impl AccountDiff {
    /// Find delta for a specific window by name
    pub fn delta_for_window(&self, window_name: &str) -> Option<f64> {
        self.window_diffs
            .iter()
            .find(|w| w.name == window_name)
            .map(|w| w.delta)
    }
}

/// Global fleet scheduling insights for bottom statusbar summary.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetInsight {
    /// Best available account with highest remaining headroom (label, remaining_percent).
    pub best_ready: Option<(String, f64)>,
    /// Earliest upcoming reset for depleted/consumed quotas (account_label, window_name, time_str).
    pub next_reset: Option<(String, String, String)>,
}

pub fn compute_fleet_insights(quotas: &[AccountQuota]) -> FleetInsight {
    let now = Utc::now();
    let valid: Vec<&AccountQuota> = quotas
        .iter()
        .filter(|q| q.error.is_none() && !q.windows.is_empty())
        .collect();

    // 1. Best ready account (account whose bottleneck remaining is the highest)
    let best_ready = valid
        .iter()
        .filter_map(|q| q.bottleneck().map(|b| (&q.label, b.remaining_percent)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(label, pct)| (label.clone(), pct));

    // 2. Earliest reset among active/consumed windows (< 95%)
    let mut upcoming: Vec<(&str, &str, chrono::Duration, String)> = Vec::new();
    for q in &valid {
        for w in &q.windows {
            if w.remaining_percent < 95.0 {
                if let Some(reset) = w.resets_at {
                    if reset > now {
                        let duration = reset.signed_duration_since(now);
                        upcoming.push((&q.label, &w.name, duration, w.format_reset_time()));
                    }
                }
            }
        }
    }
    upcoming.sort_by_key(|item| item.2);
    let next_reset = upcoming
        .first()
        .map(|(acc, win, _, time)| (acc.to_string(), win.to_string(), time.clone()));

    FleetInsight {
        best_ready,
        next_reset,
    }
}

pub fn compute_quota_diffs(
    previous: &[AccountQuota],
    current: &[AccountQuota],
) -> std::collections::HashMap<String, AccountDiff> {
    use std::collections::HashMap;
    let mut diff_map = HashMap::new();

    let prev_map: HashMap<&str, &AccountQuota> = previous
        .iter()
        .map(|q| (q.account_id.as_str(), q))
        .collect();

    for curr in current {
        let mut window_diffs = Vec::new();
        let mut is_consuming = false;
        let mut is_replenished = false;

        if let Some(prev) = prev_map.get(curr.account_id.as_str()) {
            let prev_windows: HashMap<&str, f64> = prev
                .windows
                .iter()
                .map(|w| (w.name.as_str(), w.remaining_percent))
                .collect();

            for curr_w in &curr.windows {
                if let Some(&prev_pct) = prev_windows.get(curr_w.name.as_str()) {
                    let delta = curr_w.remaining_percent - prev_pct;
                    if delta <= -0.05 {
                        is_consuming = true;
                    } else if delta >= 5.0 {
                        is_replenished = true;
                    }
                    window_diffs.push(WindowDiff {
                        name: curr_w.name.clone(),
                        previous_percent: prev_pct,
                        current_percent: curr_w.remaining_percent,
                        delta,
                    });
                }
            }
        }

        diff_map.insert(
            curr.account_id.clone(),
            AccountDiff {
                account_id: curr.account_id.clone(),
                is_consuming,
                is_replenished,
                window_diffs,
            },
        );
    }

    diff_map
}
