use reqwest::Client;
use serde_json::Value;

use super::super::time::{normalize_epoch_ms, parse_reset_timestamp};
use super::super::util::{json_f64, origin_from_url, percent_pair};
use super::super::{ProviderQuotaProbeResult, ProviderQuotaTarget, QuotaWindow};
use super::get_json;

pub(in crate::quota) async fn probe_minimax(
    http: &Client,
    target: &ProviderQuotaTarget,
    now_ms: i64,
) -> ProviderQuotaProbeResult {
    let url = token_plan_url(target.provider_id.as_str(), target.base_url.as_deref());

    let body = match get_json(http, &url, &target.api_key).await {
        Ok(json) => json,
        Err(err) => {
            return ProviderQuotaProbeResult::error_result(&target.provider_id, now_ms, err);
        }
    };

    if let Some(status_code) = body
        .pointer("/base_resp/status_code")
        .and_then(Value::as_i64)
        && status_code != 0
    {
        let msg = body
            .pointer("/base_resp/status_msg")
            .and_then(Value::as_str)
            .unwrap_or("MiniMax Token Plan query rejected");
        return ProviderQuotaProbeResult::error_result(
            &target.provider_id,
            now_ms,
            format!("MiniMax ({status_code}): {msg}"),
        );
    }

    let data = body.get("data").unwrap_or(&body);
    let mut windows = Vec::new();
    push_minimax_window(
        &mut windows,
        "interval_5h",
        "5-Hour Token Plan",
        data.get("current_interval_total_count"),
        data.get("current_interval_usage_count"),
        data.get("current_interval_reset_time"),
        now_ms,
    );
    push_minimax_window(
        &mut windows,
        "weekly",
        "Weekly Token Plan",
        data.get("current_weekly_total_count"),
        data.get("current_weekly_usage_count"),
        data.get("current_weekly_reset_time"),
        now_ms,
    );

    let mut warnings = Vec::new();
    if windows.is_empty() {
        warnings.push("MiniMax quota response contained no usable fields".to_owned());
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

fn push_minimax_window(
    windows: &mut Vec<QuotaWindow>,
    id: &str,
    label: &str,
    total_val: Option<&Value>,
    remains_val: Option<&Value>,
    reset_val: Option<&Value>,
    now_ms: i64,
) {
    let Some(total) = json_f64(total_val).filter(|v| *v > 0.0) else {
        return;
    };
    // MiniMax Token Plan reports remaining units in `*_usage_count`.
    let remaining = json_f64(remains_val).unwrap_or(total).clamp(0.0, total);
    let used = (total - remaining).max(0.0);
    let (used_percent, remaining_percent) = percent_pair(used, total);
    let reset_at_ms = reset_val.and_then(|val| {
        val.as_i64()
            .map(normalize_epoch_ms)
            .or_else(|| val.as_str().and_then(|s| parse_reset_timestamp(s, now_ms)))
    });
    windows.push(QuotaWindow {
        id: id.to_owned(),
        label: label.to_owned(),
        unit: "units".to_owned(),
        limit: Some(total),
        used: Some(used),
        remaining: Some(remaining),
        used_percent: Some(used_percent),
        remaining_percent: Some(remaining_percent),
        limited: Some(remaining <= 0.0),
        reset_at_ms,
        reset_label: None,
    });
}

/// Resolve the Token Plan `remains` endpoint, honoring any configured `base_url`.
fn token_plan_url(provider_id: &str, base_url: Option<&str>) -> String {
    if let Some(origin) = base_url
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .and_then(origin_from_url)
    {
        return format!("{origin}/v1/token_plan/remains");
    }
    if provider_id.contains("-cn") {
        "https://www.minimaxi.com/v1/token_plan/remains".to_owned()
    } else {
        "https://www.minimax.io/v1/token_plan/remains".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::token_plan_url;

    #[test]
    fn honors_any_configured_base_url_origin() {
        assert_eq!(
            token_plan_url("minimax", Some("https://api.example.test/anthropic/v1")),
            "https://api.example.test/v1/token_plan/remains"
        );
        assert_eq!(
            token_plan_url("minimax-cn", Some("https://gateway.internal:8443/proxy")),
            "https://gateway.internal:8443/v1/token_plan/remains"
        );
        assert_eq!(
            token_plan_url("minimax", None),
            "https://www.minimax.io/v1/token_plan/remains"
        );
        assert_eq!(
            token_plan_url("minimax-cn-coding-plan", None),
            "https://www.minimaxi.com/v1/token_plan/remains"
        );
        assert_eq!(
            token_plan_url("minimax", Some("   ")),
            "https://www.minimax.io/v1/token_plan/remains"
        );
    }
}
