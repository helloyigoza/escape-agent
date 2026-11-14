//! Derived integration registry: catalog providers, config overrides, remote MCPs.

use std::collections::HashSet;

use oc_antigravity::{ANTIGRAVITY_METHOD_ID, ANTIGRAVITY_PROVIDER_ID};
use oc_llm::Activation;
use oc_mcp::{McpServerConfig, OAuthSetting};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::error::ErrorResponse;

/// One registry entry, before credentials are attached as connections.
#[derive(Debug)]
pub(super) struct IntegrationRow {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) metadata: Option<Value>,
    pub(super) methods: Vec<Value>,
}

/// The credential store's view plus the derived registry.
///
/// A missing store means no credential can be read at all, so the registry is
/// served without connections instead of failing the read-only routes.
pub(super) async fn snapshot(
    state: &AppState,
) -> Result<(Vec<IntegrationRow>, Vec<Value>), ErrorResponse> {
    let credentials = match state.store() {
        Some(store) => store.list_credentials().await?,
        None => Vec::new(),
    };
    let rows = derive_rows(state, &credentials);
    Ok((rows, credentials))
}

/// Every integration this process knows about, sorted by name like upstream.
pub(super) fn derive_rows(state: &AppState, credentials: &[Value]) -> Vec<IntegrationRow> {
    let mut rows = provider_rows(state);
    rows.extend(mcp_rows(state));
    let mut seen: HashSet<String> = rows.iter().map(|row| row.id.clone()).collect();
    for credential in credentials {
        let Some(integration_id) = credential.get("integrationID").and_then(Value::as_str) else {
            continue;
        };
        if integration_id == crate::data::quotas::OPENROUTER_MGMT_INTEGRATION_ID {
            continue;
        }
        if seen.insert(integration_id.to_owned()) {
            rows.push(IntegrationRow {
                id: integration_id.to_owned(),
                name: integration_id.to_owned(),
                metadata: None,
                methods: Vec::new(),
            });
        }
    }
    rows.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    rows
}

/// Every known model provider: catalog providers (never `Disabled`) merged with
/// configured `providers`, which override the catalog name and `env` list.
///
/// Each row carries the `key` method upstream registers so the UI can connect a
/// key for any provider, plus an `env` method when the effective env list is not
/// empty.
fn provider_rows(state: &AppState) -> Vec<IntegrationRow> {
    let antigravity_enabled = state.antigravity().is_some();
    let configured = state
        .config()
        .and_then(|config| config.merged().get("providers").and_then(Value::as_object));

    let mut rows: Vec<IntegrationRow> = Vec::new();
    if let Some(catalog) = state.catalog() {
        for provider in catalog.providers() {
            if provider.activation == Activation::Disabled {
                continue;
            }
            let env: Vec<&str> = provider.env.iter().map(String::as_str).collect();
            let mut row = IntegrationRow {
                id: provider.id.clone(),
                name: non_empty(&provider.name)
                    .unwrap_or(provider.id.as_str())
                    .to_owned(),
                metadata: None,
                methods: provider_methods(&provider.id, &env, antigravity_enabled),
            };
            if let Some(config) = configured.and_then(|providers| providers.get(&provider.id)) {
                if config_disabled(config) {
                    continue;
                }
                apply_provider_config(&mut row, config, antigravity_enabled);
            }
            rows.push(row);
        }
    }
    if let Some(providers) = configured {
        for (id, config) in providers {
            if config_disabled(config) || rows.iter().any(|row| row.id == *id) {
                continue;
            }
            let mut row = IntegrationRow {
                id: id.clone(),
                name: id.clone(),
                metadata: None,
                methods: provider_methods(id, &[], antigravity_enabled),
            };
            apply_provider_config(&mut row, config, antigravity_enabled);
            rows.push(row);
        }
    }
    if antigravity_enabled
        && rows.iter().all(|row| row.id != ANTIGRAVITY_PROVIDER_ID)
        && !configured
            .and_then(|p| p.get(ANTIGRAVITY_PROVIDER_ID))
            .is_some_and(config_disabled)
    {
        rows.push(IntegrationRow {
            id: ANTIGRAVITY_PROVIDER_ID.to_owned(),
            name: "Google".to_owned(),
            metadata: None,
            methods: provider_methods(
                ANTIGRAVITY_PROVIDER_ID,
                &["GOOGLE_GENERATIVE_AI_API_KEY"],
                true,
            ),
        });
    }
    rows
}

/// The `key` method every provider exposes, `env` when declared, and Google
/// Antigravity `oauth` when the Antigravity service is attached.
fn provider_methods(id: &str, env: &[&str], antigravity_enabled: bool) -> Vec<Value> {
    let mut methods = Vec::new();
    if antigravity_enabled && id == ANTIGRAVITY_PROVIDER_ID {
        methods.push(json!({
            "id": ANTIGRAVITY_METHOD_ID,
            "type": "oauth",
            "label": "OAuth with Google (Antigravity)",
            "form": [
                {
                    "key": "projectId",
                    "title": "Google Cloud Project ID (optional)",
                    "description": "Optional Cloud Code Assist project ID. Leave blank for auto-provisioning.",
                    "required": false,
                    "type": "string"
                }
            ]
        }));
    }
    methods.push(json!({ "type": "key", "label": "Manually enter API Key" }));
    if !env.is_empty() {
        methods.push(json!({ "type": "env", "names": env }));
    }
    methods
}

/// Configured name and `env` list override the catalog row.
fn apply_provider_config(row: &mut IntegrationRow, config: &Value, antigravity_enabled: bool) {
    if let Some(name) = config
        .get("name")
        .and_then(Value::as_str)
        .and_then(non_empty)
    {
        name.clone_into(&mut row.name);
    }
    if let Some(env) = config.get("env") {
        let names: Vec<&str> = env
            .as_array()
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        row.methods = provider_methods(&row.id, &names, antigravity_enabled);
    }
}

/// Whether the provider config turns the provider off, matching the catalog
/// overlay precedence: `enabled`, then `disabled`, then `activation`.
fn config_disabled(config: &Value) -> bool {
    if let Some(enabled) = config.get("enabled").and_then(Value::as_bool) {
        return !enabled;
    }
    if config.get("disabled").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    config.get("activation").and_then(Value::as_str) == Some("disabled")
}

fn non_empty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

/// Remote MCP servers that keep OAuth enabled, keyed exactly like upstream.
fn mcp_rows(state: &AppState) -> Vec<IntegrationRow> {
    let Some(manager) = state.mcp() else {
        return Vec::new();
    };
    manager
        .servers()
        .into_iter()
        .filter_map(|(name, config)| {
            let McpServerConfig::Remote(remote) = config else {
                return None;
            };
            if matches!(remote.oauth.as_ref(), Some(OAuthSetting::Disabled(_))) {
                return None;
            }
            let id = mcp_integration_id(&name, &remote.url);
            let method = json!({ "id": id, "type": "oauth", "label": name });
            Some(IntegrationRow {
                id,
                name,
                metadata: Some(json!({ "source": "mcp" })),
                methods: vec![method],
            })
        })
        .collect()
}

/// `mcp_` plus the first 16 hex digits of a hash of `name\0url`.
///
/// Upstream uses SHA-1; this build already depends on SHA-2, so the digest
/// differs but the identity rule (name and URL, never URL alone) is the same.
fn mcp_integration_id(name: &str, url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    hasher.update([0_u8]);
    hasher.update(url.as_bytes());
    let digest = hasher.finalize();
    format!("mcp_{}", hex::encode(&digest[..8]))
}
