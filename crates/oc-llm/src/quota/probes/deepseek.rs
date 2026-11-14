use reqwest::Client;
use serde_json::Value;

use super::super::util::json_f64;
use super::super::{
    ProviderBalanceSummary, ProviderQuotaProbeResult, ProviderQuotaTarget, QuotaWindow,
};
use super::get_json;

pub(in crate::quota) async fn probe_deepseek(
    http: &Client,
    target: &ProviderQuotaTarget,
    now_ms: i64,
) -> ProviderQuotaProbeResult {
    let base = target
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("https://api.deepseek.com")
        .trim_end_matches('/');
    let root = base.strip_suffix("/v1").unwrap_or(base);
    let url = format!("{root}/user/balance");
    let body = match get_json(http, &url, &target.api_key).await {
        Ok(json) => json,
        Err(err) => {
            return ProviderQuotaProbeResult::error_result(&target.provider_id, now_ms, err);
        }
    };

    let is_available = body.get("is_available").and_then(Value::as_bool);
    let mut balances = Vec::new();
    let mut windows = Vec::new();
    if let Some(infos) = body.get("balance_infos").and_then(Value::as_array) {
        for info in infos {
            let currency = info
                .get("currency")
                .and_then(Value::as_str)
                .unwrap_or("USD")
                .to_owned();
            let total = json_f64(info.get("total_balance"));
            let granted = json_f64(info.get("granted_balance"));
            let topped_up = json_f64(info.get("topped_up_balance"));
            balances.push(ProviderBalanceSummary {
                currency: currency.clone(),
                total,
                remaining: total,
                used: None,
                topped_up,
                granted,
                is_available,
                is_free_tier: None,
            });
            if let Some(rem) = total {
                windows.push(QuotaWindow {
                    id: format!("balance_{}", currency.to_ascii_lowercase()),
                    label: format!("Account Balance ({currency})"),
                    unit: currency,
                    limit: None,
                    used: None,
                    remaining: Some(rem),
                    used_percent: None,
                    remaining_percent: None,
                    limited: is_available.map(|avail| !avail),
                    reset_at_ms: None,
                    reset_label: None,
                });
            }
        }
    }

    let mut warnings = Vec::new();
    if windows.is_empty() && balances.is_empty() {
        warnings.push("DeepSeek quota response contained no usable fields".to_owned());
    }

    ProviderQuotaProbeResult {
        provider_id: target.provider_id.clone(),
        status: if warnings.is_empty() { "ok" } else { "partial" }.to_owned(),
        fetched_at_ms: now_ms,
        windows,
        balances,
        rate_limit_rule: None,
        key_label: None,
        spend_summary: None,
        warnings,
        error: None,
    }
}
