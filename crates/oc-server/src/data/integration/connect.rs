//! Key, external and OAuth connection handlers.

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use oc_antigravity::{ANTIGRAVITY_METHOD_ID, ANTIGRAVITY_PROVIDER_ID, AntigravityError};
use oc_store::NewCredential;
use oc_types::{ApiError, LocationEnvelope};
use serde_json::{Value, json};

use crate::AppState;
use crate::data::location;
use crate::data::session::LocationQuery;
use crate::data::{no_content, query_error, require_store, schema_error};
use crate::error::ErrorResponse;

use super::bodies::{KeyConnectBody, MethodConnectBody, OAuthCompleteBody};
use super::errors::{attempt_not_found, invalid_request, not_implemented};
use super::registry::{derive_rows, snapshot};
use super::view::{credential_label, find_row, has_method};

/// `POST /api/integration/{id}/connect/key`
pub async fn connect_key(
    Path(id): Path<String>,
    State(state): State<AppState>,
    body: Result<Json<KeyConnectBody>, JsonRejection>,
) -> Result<Response, ErrorResponse> {
    let Json(body) = body.map_err(|error| ErrorResponse::from(schema_error(error)))?;
    let store = require_store(&state)?;
    let credentials = store.list_credentials().await?;
    let rows = derive_rows(&state, &credentials);
    let row = find_row(&rows, &id)?;
    if !has_method(row, "key", None) {
        return Err(ErrorResponse::from(invalid_request(
            "integration_authorization",
            format!("Key method not found: {id}"),
        )));
    }
    // The registered key method has no form, so an answer has nowhere to go.
    if body
        .answer
        .as_ref()
        .is_some_and(|answer| !answer.is_empty())
    {
        return Err(ErrorResponse::from(invalid_request(
            "integration_authorization",
            "Key method does not accept a form answer",
        )));
    }
    let label = credential_label(&row.name, body.label, &credentials, &id);
    store
        .create_credential(NewCredential {
            id: None,
            integration_id: id,
            label: Some(label),
            value: json!({ "type": "key", "key": body.key }),
            active: None,
        })
        .await?;
    Ok(no_content())
}

/// `POST /api/integration/{id}/connect/external`
pub async fn connect_external(
    Path(id): Path<String>,
    State(state): State<AppState>,
    body: Result<Json<MethodConnectBody>, JsonRejection>,
) -> Result<Response, ErrorResponse> {
    let Json(body) = body.map_err(|error| ErrorResponse::from(schema_error(error)))?;
    let store = require_store(&state)?;
    let credentials = store.list_credentials().await?;
    let rows = derive_rows(&state, &credentials);
    let row = find_row(&rows, &id)?;
    if !has_method(row, "external", Some(&body.method_id)) {
        return Err(ErrorResponse::from(invalid_request(
            "integration_authorization",
            format!("External method not found: {}", body.method_id),
        )));
    }
    if body
        .answer
        .as_ref()
        .is_some_and(|answer| !answer.is_empty())
    {
        return Err(ErrorResponse::from(invalid_request(
            "integration_authorization",
            "External method does not accept a form answer",
        )));
    }
    let label = credential_label(&row.name, body.label, &credentials, &id);
    store
        .create_credential(NewCredential {
            id: None,
            integration_id: id,
            label: Some(label),
            value: json!({ "type": "external", "methodID": body.method_id }),
            active: None,
        })
        .await?;
    Ok(no_content())
}

/// `POST /api/integration/{id}/connect/oauth`
pub async fn connect_oauth(
    Path(id): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    params: Result<Query<LocationQuery>, QueryRejection>,
    body: Result<Json<MethodConnectBody>, JsonRejection>,
) -> Response {
    let Query(params) = match params {
        Ok(params) => params,
        Err(error) => return ErrorResponse::from(query_error(error)).into_response(),
    };
    let body = match body {
        Ok(Json(body)) => body,
        Err(error) => return ErrorResponse::from(schema_error(error)).into_response(),
    };
    let (rows, _credentials) = match snapshot(&state).await {
        Ok(snapshot) => snapshot,
        Err(error) => return error.into_response(),
    };
    let row = match find_row(&rows, &id) {
        Ok(row) => row,
        Err(error) => return error.into_response(),
    };
    if !has_method(row, "oauth", Some(&body.method_id)) {
        return ErrorResponse::from(invalid_request(
            "integration_authorization",
            format!("OAuth method not found: {id}/{}", body.method_id),
        ))
        .into_response();
    }

    if id == ANTIGRAVITY_PROVIDER_ID
        && body.method_id == ANTIGRAVITY_METHOD_ID
        && let Some(svc) = state.antigravity()
    {
        let project_id = body
            .answer
            .as_ref()
            .and_then(|a| a.get("projectId"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let no_browser = body
            .answer
            .as_ref()
            .and_then(|a| a.get("noBrowser"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || body
                .answer
                .as_ref()
                .and_then(|a| a.get("loopback"))
                .and_then(Value::as_bool)
                == Some(false);
        match svc
            .start_oauth(project_id, body.label, !no_browser, None)
            .await
        {
            Ok(attempt) => {
                let resolved =
                    location::resolve(&state, &headers, None, params.directory.as_deref());
                return Json(LocationEnvelope::new(resolved.reference(), attempt)).into_response();
            }
            Err(err) => {
                return ErrorResponse::from(ApiError::new(502, "ProviderError", err.to_string()))
                    .into_response();
            }
        }
    }

    ErrorResponse::from(not_implemented("OAuth authorization is not available")).into_response()
}

/// `GET /api/integration/{id}/connect/oauth/{attemptID}`
pub async fn oauth_status(
    Path((id, attempt_id)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
    params: Result<Query<LocationQuery>, QueryRejection>,
) -> Response {
    let Query(params) = match params {
        Ok(params) => params,
        Err(error) => return ErrorResponse::from(query_error(error)).into_response(),
    };
    if id == ANTIGRAVITY_PROVIDER_ID
        && let Some(svc) = state.antigravity()
        && let Some(status) = svc.poll_oauth(&attempt_id).await
    {
        let resolved = location::resolve(&state, &headers, None, params.directory.as_deref());
        return Json(LocationEnvelope::new(resolved.reference(), status)).into_response();
    }
    ErrorResponse::from(attempt_not_found(&id, &attempt_id)).into_response()
}

/// `DELETE /api/integration/{id}/connect/oauth/{attemptID}` — cancelling an
/// attempt that does not exist is a no-op upstream.
pub async fn oauth_cancel(
    Path((id, attempt_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    if id == ANTIGRAVITY_PROVIDER_ID
        && let Some(svc) = state.antigravity()
    {
        let _ = svc.cancel_oauth(&attempt_id).await;
    }
    no_content()
}

/// `POST /api/integration/{id}/connect/oauth/{attemptID}/complete`
pub async fn oauth_complete(
    Path((id, attempt_id)): Path<(String, String)>,
    State(state): State<AppState>,
    body: Result<Json<OAuthCompleteBody>, JsonRejection>,
) -> Response {
    let body = match body {
        Ok(Json(body)) => body,
        Err(error) => return ErrorResponse::from(schema_error(error)).into_response(),
    };
    if id == ANTIGRAVITY_PROVIDER_ID
        && let Some(svc) = state.antigravity()
    {
        if svc.poll_oauth(&attempt_id).await.is_none() {
            return ErrorResponse::from(attempt_not_found(&id, &attempt_id)).into_response();
        }
        return match svc.complete_oauth(&attempt_id, body.code.as_deref()).await {
            Ok(_) => no_content(),
            Err(AntigravityError::OAuth(msg)) => {
                ErrorResponse::from(invalid_request("integration_authorization", msg))
                    .into_response()
            }
            Err(err) => ErrorResponse::from(ApiError::new(502, "ProviderError", err.to_string()))
                .into_response(),
        };
    }
    ErrorResponse::from(attempt_not_found(&id, &attempt_id)).into_response()
}
