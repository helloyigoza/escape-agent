use reqwest::Client;
use serde_json::Value;

use super::super::util::{json_f64, percent_pair};
use super::super::{
    ProviderApiSpend, ProviderBalanceSummary, ProviderQuotaProbeResult, ProviderQuotaTarget,
    QuotaWindow,
};
use super::get_json;

pub(in crate::quota) async fn probe_openrouter(
    http: &Client,
    target: &ProviderQuotaTarget,
    now_ms: i64,
) -> ProviderQuotaProbeResult {
    let base = target
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("https://openrouter.ai/api/v1")
        .trim_end_matches('/');

    let key_url = format!("{base}/key");
    let key_body = match get_json(http, &key_url, &target.api_key).await {
        Ok(json) => json,
        Err(err) => {
            return ProviderQuotaProbeResult::error_result(&target.provider_id, now_ms, err);
        }
    };

    let parsed = parse_openrouter_key_data(key_body.get("data").unwrap_or(&key_body));
    let mut windows = parsed.windows;
    let mut balances = parsed.balances;
    let mut warnings = Vec::new();

    if let Some(mgmt_key) = target
        .management_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        probe_openrouter_credits(
            http,
            base,
            mgmt_key,
            parsed.is_free_tier,
            &mut windows,
            &mut balances,
            &mut warnings,
        )
        .await;
    }

    if windows.is_empty() && balances.is_empty() && parsed.spend_summary.is_none() {
        warnings.push("OpenRouter quota response contained no usable fields".to_owned());
    }

    ProviderQuotaProbeResult {
        provider_id: target.provider_id.clone(),
        status: if warnings.is_empty() { "ok" } else { "partial" }.to_owned(),
        fetched_at_ms: now_ms,
        windows,
        balances,
        rate_limit_rule: parsed.rate_limit_rule,
        key_label: parsed.key_label,
        spend_summary: parsed.spend_summary,
        warnings,
        error: None,
    }
}

struct OpenRouterKeyData {
    key_label: Option<String>,
    rate_limit_rule: Option<String>,
    is_free_tier: Option<bool>,
    windows: Vec<QuotaWindow>,
    balances: Vec<ProviderBalanceSummary>,
    spend_summary: Option<ProviderApiSpend>,
}

fn parse_openrouter_key_data(data: &Value) -> OpenRouterKeyData {
    let key_label = data
        .get("label")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let usage = json_f64(data.get("usage"));
    let usage_daily = json_f64(data.get("usage_daily"));
    let usage_weekly = json_f64(data.get("usage_weekly"));
    let usage_monthly = json_f64(data.get("usage_monthly"));
    let limit = json_f64(data.get("limit"));
    let limit_remaining = json_f64(data.get("limit_remaining"));
    let limit_reset = data
        .get("limit_reset")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let is_free_tier = data.get("is_free_tier").and_then(Value::as_bool);
    let rate_limit_rule = data.get("rate_limit").and_then(|rl| {
        let requests = rl.get("requests").and_then(Value::as_u64)?;
        let interval = rl.get("interval").and_then(Value::as_str)?;
        Some(format!("{requests} req / {interval}"))
    });

    let mut windows = Vec::new();
    let mut balances = Vec::new();
    if let Some(limit_val) = limit.filter(|v| *v > 0.0) {
        let remaining = limit_remaining
            .unwrap_or_else(|| usage.map_or(limit_val, |used| (limit_val - used).max(0.0)));
        let used = (limit_val - remaining).max(0.0);
        let (used_percent, remaining_percent) = percent_pair(used, limit_val);
        windows.push(QuotaWindow {
            id: "key_limit".to_owned(),
            label: "API Key Credit Limit".to_owned(),
            unit: "USD".to_owned(),
            limit: Some(limit_val),
            used: Some(used),
            remaining: Some(remaining),
            used_percent: Some(used_percent),
            remaining_percent: Some(remaining_percent),
            limited: Some(remaining <= 0.0),
            reset_at_ms: None,
            reset_label: limit_reset,
        });
    }
    if limit.is_some() || limit_remaining.is_some() || is_free_tier.is_some() {
        balances.push(ProviderBalanceSummary {
            currency: "USD".to_owned(),
            total: limit,
            remaining: limit_remaining,
            used: usage,
            topped_up: None,
            granted: None,
            is_available: Some(limit_remaining.is_none_or(|rem| rem > 0.0)),
            is_free_tier,
        });
    }

    let spend_summary = if usage.is_some()
        || usage_daily.is_some()
        || usage_weekly.is_some()
        || usage_monthly.is_some()
    {
        Some(ProviderApiSpend {
            currency: "USD".to_owned(),
            total: usage,
            daily: usage_daily,
            weekly: usage_weekly,
            monthly: usage_monthly,
        })
    } else {
        None
    };

    OpenRouterKeyData {
        key_label,
        rate_limit_rule,
        is_free_tier,
        windows,
        balances,
        spend_summary,
    }
}

async fn probe_openrouter_credits(
    http: &Client,
    base: &str,
    mgmt_key: &str,
    is_free_tier: Option<bool>,
    windows: &mut Vec<QuotaWindow>,
    balances: &mut Vec<ProviderBalanceSummary>,
    warnings: &mut Vec<String>,
) {
    let credits_url = format!("{base}/credits");
    match get_json(http, &credits_url, mgmt_key).await {
        Ok(credits_body) => {
            let cdata = credits_body.get("data").unwrap_or(&credits_body);
            let total_credits = json_f64(cdata.get("total_credits"));
            let total_usage = json_f64(cdata.get("total_usage"));
            if let (Some(total), Some(used)) = (total_credits, total_usage) {
                let remaining = (total - used).max(0.0);
                balances.push(ProviderBalanceSummary {
                    currency: "USD".to_owned(),
                    total: Some(total),
                    remaining: Some(remaining),
                    used: Some(used),
                    topped_up: None,
                    granted: None,
                    is_available: Some(remaining > 0.0),
                    is_free_tier,
                });
                if total > 0.0 {
                    let (used_percent, remaining_percent) = percent_pair(used, total);
                    windows.insert(
                        0,
                        QuotaWindow {
                            id: "account_credits".to_owned(),
                            label: "Account Wallet Credits".to_owned(),
                            unit: "USD".to_owned(),
                            limit: Some(total),
                            used: Some(used),
                            remaining: Some(remaining),
                            used_percent: Some(used_percent),
                            remaining_percent: Some(remaining_percent),
                            limited: Some(remaining <= 0.0),
                            reset_at_ms: None,
                            reset_label: None,
                        },
                    );
                }
            } else {
                warnings.push("OpenRouter /credits returned unexpected shape".to_owned());
            }
        }
        Err(err) => {
            warnings.push(format!("OpenRouter wallet credits (/credits): {err}"));
        }
    }
}
