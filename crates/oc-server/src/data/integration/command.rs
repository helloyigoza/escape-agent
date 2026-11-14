//! Command authorization and experimental well-known discovery routes.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::response::Response;
use oc_types::ApiError;

use crate::AppState;
use crate::data::{no_content, schema_error};
use crate::error::ErrorResponse;

use super::bodies::{CommandConnectBody, WellKnownBody};
use super::errors::{attempt_not_found, invalid_request, not_implemented};
use super::registry::snapshot;
use super::view::{find_row, has_method};

/// `POST /api/integration/{id}/connect/command`
pub async fn command_connect(
    Path(id): Path<String>,
    State(state): State<AppState>,
    body: Result<Json<CommandConnectBody>, JsonRejection>,
) -> ErrorResponse {
    let body = match body {
        Ok(Json(body)) => body,
        Err(error) => return schema_error(error).into(),
    };
    let (rows, _credentials) = match snapshot(&state).await {
        Ok(snapshot) => snapshot,
        Err(error) => return error,
    };
    let row = match find_row(&rows, &id) {
        Ok(row) => row,
        Err(error) => return error,
    };
    if !has_method(row, "command", Some(&body.method_id)) {
        return invalid_request(
            "integration_authorization",
            format!("Command method not found: {id}/{}", body.method_id),
        )
        .into();
    }
    not_implemented("command authorization is not available").into()
}

/// `GET /api/integration/{id}/connect/command/{attemptID}`
pub async fn command_status(Path((id, attempt_id)): Path<(String, String)>) -> ErrorResponse {
    attempt_not_found(&id, &attempt_id).into()
}

/// `DELETE /api/integration/{id}/connect/command/{attemptID}` — cancelling an
/// attempt that does not exist is a no-op upstream.
pub async fn command_cancel(Path((_id, _attempt_id)): Path<(String, String)>) -> Response {
    no_content()
}

/// `GET /api/experimental/integration/wellknown`
///
/// Upstream only exposes `POST` on this path; the read form reports the same
/// missing configuration instead of pretending discovery is available.
pub async fn wellknown_status() -> ErrorResponse {
    ApiError::service_unavailable("well-known integration discovery is not configured").into()
}

/// `POST /api/experimental/integration/wellknown`
///
/// The body is validated, then discovery is refused: no well-known source
/// engine is configured.
pub async fn wellknown_add(
    body: Result<Json<WellKnownBody>, JsonRejection>,
) -> Result<Response, ErrorResponse> {
    let Json(body) = body.map_err(|error| ErrorResponse::from(schema_error(error)))?;
    let parsed = url::Url::parse(&body.url).map_err(|error| {
        ErrorResponse::from(invalid_request(
            "well_known_discovery",
            format!("invalid integration URL: {error}"),
        ))
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ErrorResponse::from(invalid_request(
            "well_known_discovery",
            "integration URL must use http or https",
        )));
    }
    Err(ErrorResponse::from(ApiError::service_unavailable(
        "well-known integration discovery is not configured",
    )))
}
