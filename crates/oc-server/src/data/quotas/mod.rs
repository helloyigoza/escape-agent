//! Unified multi-provider quota, balance, rate-limit, and local usage routes.
//!
//! - `GET /api/quotas` (`?refresh=true|false`)
//! - `POST /api/quotas/refresh`
//! - `PUT /api/quotas/openrouter/management-key`
//! - `DELETE /api/quotas/openrouter/management-key`
//!
//! This module is the facade: it owns the cache handle, the router, the wire
//! bodies and the handlers; probing, credential resolution and the local usage
//! scan live in the sibling submodules.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::{get, post, put};
use oc_llm::ProviderQuotaProbeResult;
use oc_types::{ApiError, DataEnvelope};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::RwLock;

use crate::AppState;
use crate::data::{no_content, query_error, require_store, schema_error};
use crate::error::ErrorResponse;

mod aggregate;
mod antigravity;
mod credentials;
mod openrouter;
mod providers;
mod types;
mod usage;

use aggregate::build_unified_quotas;
use openrouter::{
    clear_openrouter_management_key, env_managed_error, is_env_managed,
    save_openrouter_management_key,
};
use usage::UsageCache;

/// Synthetic credential integration ID used when only `OPENROUTER_API_KEY` env var
/// is present and the user saves an `OpenRouter` Management Key via `/api/quotas`.
pub(crate) const OPENROUTER_MGMT_INTEGRATION_ID: &str = "openrouter-management";

pub(super) const CACHE_TTL_MS: i64 = 60_000;

/// Cached live provider probes and local usage rollup held in [`AppState`].
#[derive(Debug, Default)]
pub struct QuotaCache {
    pub(in crate::data::quotas) probes: BTreeMap<String, ProviderQuotaProbeResult>,
    pub(in crate::data::quotas) usage: UsageCache,
}

impl QuotaCache {
    /// Remove a cached provider entry so the next read re-probes upstream.
    pub fn invalidate(&mut self, provider_id: &str) {
        self.probes.remove(provider_id);
    }

    /// Drop every cached probe and the usage rollup so the next read
    /// recomputes both halves.
    pub fn invalidate_all(&mut self) {
        self.probes.clear();
        self.usage = UsageCache::default();
    }
}

/// Protected quota/usage control-plane routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/quotas", get(get_quotas))
        .route("/api/quotas/refresh", post(refresh_quotas))
        .route(
            "/api/quotas/openrouter/management-key",
            put(set_openrouter_management_key).delete(delete_openrouter_management_key),
        )
}

#[derive(Debug, Default, Deserialize)]
pub struct QuotasQuery {
    #[serde(default)]
    refresh: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementKeyBody {
    key: String,
}

/// `GET /api/quotas?refresh=true|false`
pub async fn get_quotas(
    State(state): State<AppState>,
    params: Result<Query<QuotasQuery>, QueryRejection>,
) -> Result<Json<DataEnvelope<Value>>, ErrorResponse> {
    let Query(params) = params.map_err(|error| ErrorResponse::from(query_error(error)))?;
    let payload = build_unified_quotas(&state, params.refresh.unwrap_or(false)).await?;
    let value = serde_json::to_value(payload).map_err(|err| ApiError::internal(err.to_string()))?;
    Ok(Json(DataEnvelope::new(value)))
}

/// `POST /api/quotas/refresh`
pub async fn refresh_quotas(
    State(state): State<AppState>,
) -> Result<Json<DataEnvelope<Value>>, ErrorResponse> {
    state.quota_cache().write().await.invalidate_all();
    let payload = build_unified_quotas(&state, true).await?;
    let value = serde_json::to_value(payload).map_err(|err| ApiError::internal(err.to_string()))?;
    Ok(Json(DataEnvelope::new(value)))
}

/// `PUT /api/quotas/openrouter/management-key`
pub async fn set_openrouter_management_key(
    State(state): State<AppState>,
    body: Result<Json<ManagementKeyBody>, JsonRejection>,
) -> Result<Json<DataEnvelope<Value>>, ErrorResponse> {
    let Json(body) = body.map_err(|error| ErrorResponse::from(schema_error(error)))?;
    let key = body.key.trim();
    if key.is_empty() {
        return Err(ErrorResponse::from(ApiError::new(
            400,
            "InvalidRequestError",
            "Management key cannot be empty",
        )));
    }
    let store = require_store(&state)?;
    save_openrouter_management_key(store, key).await?;
    state.quota_cache().write().await.invalidate("openrouter");
    Ok(Json(DataEnvelope::new(json!({ "configured": true }))))
}

/// `DELETE /api/quotas/openrouter/management-key`
pub async fn delete_openrouter_management_key(
    State(state): State<AppState>,
) -> Result<Response, ErrorResponse> {
    let store = require_store(&state)?;
    let credentials = store.list_credentials().await?;
    if is_env_managed(&credentials) {
        return Err(ErrorResponse::from(env_managed_error()));
    }
    clear_openrouter_management_key(store).await?;
    state.quota_cache().write().await.invalidate("openrouter");
    Ok(no_content())
}

/// Shared cache handle helper used by [`AppState`].
pub(crate) fn new_quota_cache() -> Arc<RwLock<QuotaCache>> {
    Arc::new(RwLock::new(QuotaCache::default()))
}
