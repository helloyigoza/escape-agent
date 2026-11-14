//! `OpenRouter` management-key scope: resolution order, persistence and
//! clearing.

use oc_config::ConfigService;
use oc_store::{CredentialPatch, NewCredential, Store};
use oc_types::ApiError;
use serde_json::{Map, Value, json};

use crate::error::ErrorResponse;

use super::OPENROUTER_MGMT_INTEGRATION_ID;
use super::credentials::credential_key_for;
use super::types::ManagementKeySource;

const OPENROUTER_MGMT_CRED_ID: &str = "cred_openrouter_mgmt";

#[derive(Debug, Clone)]
pub(super) struct OpenrouterManagementKey {
    pub(super) key: String,
    pub(super) source: ManagementKeySource,
}

pub(super) fn resolve_openrouter_management_key(
    credentials: &[Value],
    config: Option<&ConfigService>,
) -> Option<OpenrouterManagementKey> {
    if let Ok(env_key) = std::env::var("OPENROUTER_MANAGEMENT_KEY")
        && !env_key.trim().is_empty()
    {
        return Some(OpenrouterManagementKey {
            key: env_key,
            source: ManagementKeySource::Env,
        });
    }
    for entry in credentials {
        if entry.get("integrationID").and_then(Value::as_str) == Some("openrouter")
            && let Some(key) = entry
                .pointer("/value/metadata/managementKey")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        {
            return Some(OpenrouterManagementKey {
                key: key.to_owned(),
                source: ManagementKeySource::Credential,
            });
        }
    }
    if let Some(key) = credential_key_for(OPENROUTER_MGMT_INTEGRATION_ID, credentials) {
        return Some(OpenrouterManagementKey {
            key,
            source: ManagementKeySource::Credential,
        });
    }
    config
        .and_then(|cfg| {
            cfg.merged()
                .pointer("/providers/openrouter/settings/managementKey")
        })
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|key| OpenrouterManagementKey {
            key: key.to_owned(),
            source: ManagementKeySource::Config,
        })
}

/// Whether `OPENROUTER_MANAGEMENT_KEY` currently owns the key.
pub(super) fn is_env_managed(credentials: &[Value]) -> bool {
    resolve_openrouter_management_key(credentials, None)
        .is_some_and(|resolved| resolved.source == ManagementKeySource::Env)
}

/// The 409 body the UI reads as `data.kind == "env_managed"`.
pub(super) fn env_managed_error() -> ApiError {
    ApiError::new(
        409,
        "InvalidRequestError",
        "The OpenRouter management key is supplied by OPENROUTER_MANAGEMENT_KEY and cannot be cleared from stored credentials",
    )
    .with("data", json!({ "kind": "env_managed" }))
}

/// Pick the credential a saved management key is written to.
///
/// First active entry (the store lists by ascending id), else the lowest id, so
/// ties never depend on iteration direction.
fn select_openrouter_credential(credentials: &[Value]) -> Option<&Value> {
    let mut fallback: Option<&Value> = None;
    for entry in credentials
        .iter()
        .filter(|c| c.get("integrationID").and_then(Value::as_str) == Some("openrouter"))
    {
        if entry.get("active").and_then(Value::as_bool) == Some(true) {
            return Some(entry);
        }
        let lower_id = match fallback {
            None => true,
            Some(current) => {
                let current_id = current.get("id").and_then(Value::as_str).unwrap_or("");
                let candidate_id = entry.get("id").and_then(Value::as_str).unwrap_or("");
                candidate_id < current_id
            }
        };
        if lower_id {
            fallback = Some(entry);
        }
    }
    fallback
}

pub(super) async fn save_openrouter_management_key(
    store: &Store,
    key: &str,
) -> Result<(), ErrorResponse> {
    let credentials = store.list_credentials().await?;
    let mut updated_existing = false;
    if let Some(existing) = select_openrouter_credential(&credentials)
        && let Some(id) = existing.get("id").and_then(Value::as_str)
        && let Some(mut val) = existing.get("value").cloned()
        && let Some(obj) = val.as_object_mut()
    {
        let meta = obj
            .entry("metadata")
            .or_insert_with(|| Value::Object(Map::new()));
        if !meta.is_object() {
            *meta = Value::Object(Map::new());
        }
        if let Some(meta_map) = meta.as_object_mut() {
            meta_map.insert("managementKey".to_owned(), Value::String(key.to_owned()));
            store
                .update_credential(
                    id,
                    CredentialPatch {
                        value: Some(val),
                        ..CredentialPatch::default()
                    },
                )
                .await?;
            updated_existing = true;
        }
    }

    if !updated_existing {
        let value = json!({ "type": "key", "key": key });
        if store
            .get_credential(OPENROUTER_MGMT_CRED_ID)
            .await?
            .is_some()
        {
            store
                .update_credential(
                    OPENROUTER_MGMT_CRED_ID,
                    CredentialPatch {
                        value: Some(value),
                        ..CredentialPatch::default()
                    },
                )
                .await?;
        } else {
            store
                .create_credential(NewCredential {
                    id: Some(OPENROUTER_MGMT_CRED_ID.to_owned()),
                    integration_id: OPENROUTER_MGMT_INTEGRATION_ID.to_owned(),
                    label: Some("OpenRouter Management Key".to_owned()),
                    value,
                    active: Some(true),
                })
                .await?;
        }
    }
    Ok(())
}

pub(super) async fn clear_openrouter_management_key(store: &Store) -> Result<(), ErrorResponse> {
    let credentials = store.list_credentials().await?;
    for entry in &credentials {
        if entry.get("integrationID").and_then(Value::as_str) != Some("openrouter") {
            continue;
        }
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(mut val) = entry.get("value").cloned() else {
            continue;
        };
        if let Some(meta_map) = val.pointer_mut("/metadata").and_then(Value::as_object_mut)
            && meta_map.remove("managementKey").is_some()
        {
            if meta_map.is_empty()
                && let Some(obj) = val.as_object_mut()
            {
                obj.remove("metadata");
            }
            store
                .update_credential(
                    id,
                    CredentialPatch {
                        value: Some(val),
                        ..CredentialPatch::default()
                    },
                )
                .await?;
        }
    }
    let _ = store.delete_credential(OPENROUTER_MGMT_CRED_ID).await?;
    Ok(())
}
