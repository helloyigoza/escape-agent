use reqwest::Client;
use serde_json::Value;

use super::super::util::json_f64;
use super::super::{
    ProviderBalanceSummary, ProviderQuotaProbeResult, ProviderQuotaTarget, QuotaWindow,
};
use super::get_json;

pub(in crate::quota) async fn probe_siliconflow(
    http: &Client,
    target: &ProviderQuotaTarget,
    now_ms: i64,
) -> ProviderQuotaProbeResult {
    let default_base = if target.provider_id == "siliconflow-cn" {
        "https://api.siliconflow.cn/v1"
    } else {
        "https://api.siliconflow.com/v1"
    };
    let base = target
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_base)
        .trim_end_matches('/');
    let url = format!("{base}/user/info");
    let body = match get_json(http, &url, &target.api_key).await {
        Ok(json) => json,
        Err(err) => {
            return ProviderQuotaProbeResult::error_result(&target.provider_id, now_ms, err);
        }
    };

    let data = body.get("data").unwrap_or(&body);
    let balance = json_f64(data.get("balance"));
    let charge_balance = json_f64(data.get("chargeBalance"));
    let total_balance = json_f64(data.get("totalBalance")).or(balance);
    let is_available = data
        .get("status")
        .and_then(Value::as_str)
        .map(|s| !s.eq_ignore_ascii_case("disabled"))
        .or_else(|| total_balance.map(|v| v > 0.0));
    let currency = if base.contains(".cn") || target.provider_id.ends_with("-cn") {
        "CNY"
    } else {
        "USD"
    }
    .to_owned();

    let mut balances = Vec::new();
    let mut windows = Vec::new();
    if let Some(rem) = total_balance {
        let granted = match (total_balance, charge_balance) {
            (Some(tot), Some(charge)) if tot >= charge => Some((tot - charge).max(0.0)),
            _ => None,
        };
        balances.push(ProviderBalanceSummary {
            currency: currency.clone(),
            total: total_balance,
            remaining: Some(rem),
            used: None,
            topped_up: charge_balance,
            granted,
            is_available,
            is_free_tier: None,
        });
        windows.push(QuotaWindow {
            id: "balance".to_owned(),
            label: format!("Account Balance ({currency})"),
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

    let mut warnings = Vec::new();
    if windows.is_empty() && balances.is_empty() {
        warnings.push("SiliconFlow quota response contained no usable fields".to_owned());
    }

    ProviderQuotaProbeResult {
        provider_id: target.provider_id.clone(),
        status: if warnings.is_empty() { "ok" } else { "partial" }.to_owned(),
        fetched_at_ms: now_ms,
        windows,
        balances,
        rate_limit_rule: None,
        key_label: data
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        spend_summary: None,
        warnings,
        error: None,
    }
}
