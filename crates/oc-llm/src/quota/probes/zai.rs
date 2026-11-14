use reqwest::Client;
use serde_json::Value;

use super::super::time::normalize_epoch_ms;
use super::super::util::{json_f64, origin_from_url, percent_pair};
use super::super::{ProviderQuotaProbeResult, ProviderQuotaTarget, QuotaWindow};
use super::get_json;

pub(in crate::quota) async fn probe_zai(
    http: &Client,
    target: &ProviderQuotaTarget,
    now_ms: i64,
) -> ProviderQuotaProbeResult {
    let default_origin = if target.provider_id.starts_with("zhipuai") {
        "https://open.bigmodel.cn"
    } else {
        "https://api.z.ai"
    };
    let origin = target
        .base_url
        .as_deref()
        .and_then(origin_from_url)
        .unwrap_or_else(|| default_origin.to_owned());
    let url = format!("{origin}/api/monitor/usage/quota/limit");
    let body = match get_json(http, &url, &target.api_key).await {
        Ok(json) => json,
        Err(err) => {
            return ProviderQuotaProbeResult::error_result(&target.provider_id, now_ms, err);
        }
    };

    if body.get("success").and_then(Value::as_bool) == Some(false) {
        let message = body
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("Z.AI quota API returned an unsuccessful response");
        return ProviderQuotaProbeResult::error_result(&target.provider_id, now_ms, message);
    }

    let mut windows = Vec::new();
    let limits = body
        .pointer("/data/limits")
        .and_then(Value::as_array)
        .or_else(|| body.get("limits").and_then(Value::as_array));

    if let Some(items) = limits {
        for (idx, item) in items.iter().enumerate() {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("LIMIT");
            let unit_code = item.get("unit").and_then(Value::as_i64);
            let number = item.get("number").and_then(Value::as_i64);
            let limit = json_f64(item.get("usage"));
            let used = json_f64(item.get("currentValue"));
            let remaining = json_f64(item.get("remaining"));
            let raw_pct = json_f64(item.get("percentage"));
            let reset_at_ms = item
                .get("nextResetTime")
                .and_then(Value::as_i64)
                .map(normalize_epoch_ms);

            let (used_percent, remaining_percent) = match (raw_pct, used, limit) {
                (Some(pct), _, _) => {
                    let clamped = pct.clamp(0.0, 100.0);
                    (Some(clamped), Some((100.0 - clamped).max(0.0)))
                }
                (None, Some(u), Some(l)) if l > 0.0 => {
                    let (up, rp) = percent_pair(u, l);
                    (Some(up), Some(rp))
                }
                _ => (None, None),
            };

            let (label, unit) = match kind {
                "TOKENS_LIMIT" => {
                    let label = match (unit_code, number) {
                        (Some(3), Some(n)) => format!("{n}-Hour Token Quota"),
                        (Some(6), Some(1)) => "Weekly Token Quota".to_owned(),
                        (Some(6), Some(n)) => format!("{n}-Week Token Quota"),
                        _ => "Token Quota".to_owned(),
                    };
                    (label, "tokens".to_owned())
                }
                "TIME_LIMIT" => ("Monthly Tool / Search Quota".to_owned(), "calls".to_owned()),
                other => (other.to_owned(), "units".to_owned()),
            };

            windows.push(QuotaWindow {
                id: format!("{}_{}", kind.to_ascii_lowercase(), idx),
                label,
                unit,
                limit,
                used,
                remaining,
                used_percent,
                remaining_percent,
                limited: remaining_percent.map(|rp| rp <= 0.0),
                reset_at_ms,
                reset_label: None,
            });
        }
    }

    let mut warnings = Vec::new();
    if windows.is_empty() {
        warnings.push("Z.AI quota response contained no usable fields".to_owned());
    }

    ProviderQuotaProbeResult {
        provider_id: target.provider_id.clone(),
        status: if warnings.is_empty() { "ok" } else { "partial" }.to_owned(),
        fetched_at_ms: now_ms,
        windows,
        balances: Vec::new(),
        rate_limit_rule: None,
        key_label: None,
        spend_summary: None,
        warnings,
        error: None,
    }
}
