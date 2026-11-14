use reqwest::Client;
use serde_json::Value;

use super::super::time::parse_rfc3339_ms;
use super::super::util::{json_f64, origin_from_url};
use super::super::{ProviderQuotaProbeResult, ProviderQuotaTarget, QuotaWindow};
use super::get_json;

/// Origin used when the target carries no configured `base_url`.
const DEFAULT_ORIGIN: &str = "https://opencode.ai";

/// The `opencode-go` usage endpoint path, appended to the resolved origin.
const USAGE_PATH: &str = "/zen/go/v1/usage";

/// The three percent-only usage windows, in report order.
const WINDOWS: [(&str, &str); 3] = [
    ("rolling", "5-Hour Rolling"),
    ("weekly", "Weekly"),
    ("monthly", "Monthly"),
];

pub(in crate::quota) async fn probe_opencode_go(
    http: &Client,
    target: &ProviderQuotaTarget,
    now_ms: i64,
) -> ProviderQuotaProbeResult {
    let url = usage_url(target.base_url.as_deref());
    let body = match get_json(http, &url, &target.api_key).await {
        Ok(json) => json,
        Err(err) => {
            return ProviderQuotaProbeResult::error_result(&target.provider_id, now_ms, err);
        }
    };

    let mut windows = Vec::new();
    if let Some(usage) = body.get("usage").and_then(Value::as_object) {
        for (id, label) in WINDOWS {
            let Some(raw) = usage.get(id) else {
                continue;
            };
            if let Some(window) = percent_window(id, label, raw) {
                windows.push(window);
            }
        }
    }

    let mut warnings = Vec::new();
    if windows.is_empty() {
        warnings.push("OpenCode Go usage response contained no usable fields".to_owned());
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

/// Build one percent window; `percent` is the only usable field the API reports.
fn percent_window(id: &str, label: &str, raw: &Value) -> Option<QuotaWindow> {
    let used_percent = json_f64(raw.get("percent"))?.clamp(0.0, 100.0);
    let remaining_percent = (100.0 - used_percent).clamp(0.0, 100.0);
    let reset_at_ms = raw
        .get("resetsAt")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_ms);
    Some(QuotaWindow {
        id: id.to_owned(),
        label: label.to_owned(),
        unit: "percent".to_owned(),
        limit: Some(100.0),
        used: Some(used_percent),
        remaining: Some(remaining_percent),
        used_percent: Some(used_percent),
        remaining_percent: Some(remaining_percent),
        limited: Some(used_percent >= 100.0),
        reset_at_ms,
        reset_label: None,
    })
}

/// Resolve the usage endpoint: any configured `base_url` is reduced to its
/// origin, so the catalog path (`/inference/go/openai/v1`) never leaks in.
fn usage_url(base_url: Option<&str>) -> String {
    let origin = base_url
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .and_then(origin_from_url)
        .unwrap_or_else(|| DEFAULT_ORIGIN.to_owned());
    format!("{origin}{USAGE_PATH}")
}

#[cfg(test)]
mod tests {
    use super::usage_url;

    #[test]
    fn reduces_any_configured_base_url_to_its_origin() {
        assert_eq!(
            usage_url(Some("https://example.test/whatever")),
            "https://example.test/zen/go/v1/usage"
        );
        assert_eq!(
            usage_url(Some("https://opencode.ai/inference/go/openai/v1")),
            "https://opencode.ai/zen/go/v1/usage"
        );
        assert_eq!(
            usage_url(Some("http://gateway.internal:8443/proxy/v1")),
            "http://gateway.internal:8443/zen/go/v1/usage"
        );
        assert_eq!(usage_url(None), "https://opencode.ai/zen/go/v1/usage");
        assert_eq!(
            usage_url(Some("   ")),
            "https://opencode.ai/zen/go/v1/usage"
        );
    }
}
