use serde_json::Value;

pub(super) fn origin_from_url(raw: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(raw.trim()).ok()?;
    let host = parsed.host_str()?;
    let port = parsed.port().map_or_else(String::new, |p| format!(":{p}"));
    Some(format!("{}://{}{}", parsed.scheme(), host, port))
}

pub(super) fn json_f64(value: Option<&Value>) -> Option<f64> {
    let val = value?;
    if let Some(num) = val.as_f64() {
        return num.is_finite().then_some(num);
    }
    val.as_str()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|num| num.is_finite())
}

pub(super) fn percent_pair(used: f64, limit: f64) -> (f64, f64) {
    if !limit.is_finite() || limit <= 0.0 {
        return (0.0, 100.0);
    }
    let used_pct = ((used / limit) * 100.0).clamp(0.0, 100.0);
    let rem_pct = (100.0 - used_pct).clamp(0.0, 100.0);
    (used_pct, rem_pct)
}
