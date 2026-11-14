use reqwest::Client;

use super::super::util::json_f64;
use super::super::{
    ProviderBalanceSummary, ProviderQuotaProbeResult, ProviderQuotaTarget, QuotaWindow,
};
use super::get_json;

pub(in crate::quota) async fn probe_moonshot(
    http: &Client,
    target: &ProviderQuotaTarget,
    now_ms: i64,
) -> ProviderQuotaProbeResult {
    let default_base = if target.provider_id == "moonshotai-cn" {
        "https://api.moonshot.cn/v1"
    } else {
        "https://api.moonshot.ai/v1"
    };
    let base = target
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_base)
        .trim_end_matches('/');
    let url = format!("{base}/users/me/balance");
    let body = match get_json(http, &url, &target.api_key).await {
        Ok(json) => json,
        Err(err) => {
            return ProviderQuotaProbeResult::error_result(&target.provider_id, now_ms, err);
        }
    };

    let data = body.get("data").unwrap_or(&body);
    let available = json_f64(data.get("available_balance"));
    let cash = json_f64(data.get("cash_balance"));
    let voucher = json_f64(data.get("voucher_balance"));
    let currency = if base.contains(".cn") || target.provider_id.ends_with("-cn") {
        "CNY"
    } else {
        "USD"
    }
    .to_owned();

    let mut balances = Vec::new();
    let mut windows = Vec::new();
    if available.is_some() || cash.is_some() || voucher.is_some() {
        balances.push(ProviderBalanceSummary {
            currency: currency.clone(),
            total: available,
            remaining: available,
            used: None,
            topped_up: cash,
            granted: voucher,
            is_available: available.map(|val| val > 0.0),
            is_free_tier: None,
        });
        if let Some(rem) = available {
            windows.push(QuotaWindow {
                id: "balance".to_owned(),
                label: format!("Available Balance ({currency})"),
                unit: currency,
                limit: None,
                used: None,
                remaining: Some(rem),
                used_percent: None,
                remaining_percent: None,
                limited: Some(rem <= 0.0),
                reset_at_ms: None,
                reset_label: None,
            });
        }
    }

    let mut warnings = Vec::new();
    if windows.is_empty() && balances.is_empty() {
        warnings.push("Moonshot quota response contained no usable fields".to_owned());
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
