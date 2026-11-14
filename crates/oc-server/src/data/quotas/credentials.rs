//! Credential, config and environment resolution for provider probes.

use oc_config::ConfigService;
use oc_llm::Catalog;
use serde_json::Value;

pub(super) fn has_direct_google_api_key(
    catalog: Option<&Catalog>,
    config: Option<&ConfigService>,
    credentials: &[Value],
) -> bool {
    if credentials.iter().any(|c| {
        c.get("integrationID").and_then(Value::as_str) == Some("google")
            && c.get("active").and_then(Value::as_bool) == Some(true)
            && c.pointer("/value/type").and_then(Value::as_str) != Some("oauth")
    }) {
        return true;
    }
    if let Some(provider) = catalog.and_then(|cat| cat.provider("google"))
        && provider
            .env
            .iter()
            .any(|name| std::env::var(name).is_ok_and(|val| !val.is_empty()))
    {
        return true;
    }
    config
        .and_then(|cfg| cfg.merged().pointer("/providers/google/settings/apiKey"))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
}

pub(super) fn resolve_connection_source(
    provider_id: &str,
    connected: bool,
    catalog: Option<&Catalog>,
    config: Option<&ConfigService>,
    credentials: &[Value],
) -> String {
    if !connected {
        return "history".to_owned();
    }
    if let Some(method) = find_credential_method(provider_id, credentials).or_else(|| {
        (provider_id == "opencode-go")
            .then(|| find_credential_method("opencode", credentials))
            .flatten()
    }) {
        return if method == "oauth" {
            "oauth".to_owned()
        } else {
            "credential".to_owned()
        };
    }
    if let Some(provider) = catalog.and_then(|cat| cat.provider(provider_id))
        && provider
            .env
            .iter()
            .any(|name| std::env::var(name).is_ok_and(|val| !val.is_empty()))
    {
        return "env".to_owned();
    }
    if config
        .and_then(|cfg| cfg.merged().get("providers")?.get(provider_id))
        .is_some()
    {
        return "config".to_owned();
    }
    "unknown".to_owned()
}

/// Credential kind (`oauth` / `key` / `external`) for `provider_id`, from an
/// **active** row only. A deactivated credential never answers: it must not
/// stand in for a probe key or for the reported connection source. `None` when
/// no active credential exists.
fn find_credential_method<'a>(provider_id: &str, credentials: &'a [Value]) -> Option<&'a str> {
    for entry in credentials {
        if entry.get("integrationID").and_then(Value::as_str) != Some(provider_id) {
            continue;
        }
        if entry.get("active").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let Some(kind) = entry.pointer("/value/type").and_then(Value::as_str) else {
            continue;
        };
        return Some(kind);
    }
    None
}

pub(super) fn resolve_provider_api_key(
    provider_id: &str,
    catalog: Option<&Catalog>,
    config: Option<&ConfigService>,
    credentials: &[Value],
) -> Option<String> {
    if let Some(key) = credential_key_for(provider_id, credentials) {
        return Some(key);
    }
    if provider_id == "opencode-go"
        && let Some(key) = credential_key_for("opencode", credentials)
    {
        return Some(key);
    }
    if let Some(key) = config
        .and_then(|cfg| {
            cfg.merged()
                .pointer(&format!("/providers/{provider_id}/settings/apiKey"))
        })
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Some(key.to_owned());
    }
    if let Some(provider) = catalog.and_then(|cat| cat.provider(provider_id)) {
        for var in &provider.env {
            if let Ok(val) = std::env::var(var)
                && !val.trim().is_empty()
            {
                return Some(val);
            }
        }
    }
    None
}

/// The API key of an **active** credential for `provider_id`.
///
/// A deactivated credential must never be used as a probe key, so there is no
/// fallback to it: callers get `None` and the provider is simply not probed.
pub(super) fn credential_key_for(provider_id: &str, credentials: &[Value]) -> Option<String> {
    for entry in credentials {
        if entry.get("integrationID").and_then(Value::as_str) != Some(provider_id) {
            continue;
        }
        if entry.get("active").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let Some(value) = entry.get("value") else {
            continue;
        };
        let key = match value.get("type").and_then(Value::as_str) {
            Some("oauth") => value
                .get("access")
                .or_else(|| value.get("access_token"))
                .or_else(|| value.get("key")),
            _ => value
                .get("key")
                .or_else(|| value.get("access"))
                .or_else(|| value.get("access_token")),
        }
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
        if let Some(key) = key {
            return Some(key.to_owned());
        }
    }
    None
}

pub(super) fn resolve_provider_base_url(
    provider_id: &str,
    catalog: Option<&Catalog>,
    config: Option<&ConfigService>,
) -> Option<String> {
    if let Some(url) = config
        .and_then(|cfg| {
            cfg.merged()
                .pointer(&format!("/providers/{provider_id}/settings/baseURL"))
                .or_else(|| {
                    cfg.merged()
                        .pointer(&format!("/providers/{provider_id}/options/baseURL"))
                })
        })
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(url.to_owned());
    }
    if let Some(cat) = catalog {
        for model in cat.models() {
            if model.provider_id == provider_id
                && let Ok(resolved) = cat.resolve(&format!("{provider_id}/{}", model.id))
                && !resolved.base_url.trim().is_empty()
            {
                return Some(resolved.base_url);
            }
        }
    }
    None
}
