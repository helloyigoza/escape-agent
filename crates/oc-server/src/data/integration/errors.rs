//! Tagged error constructors for credential and integration endpoints.

use oc_types::ApiError;
use serde_json::json;

use crate::error::ErrorResponse;

pub(super) fn credential_not_found(id: &str) -> ErrorResponse {
    ApiError::not_found("NotFoundError", format!("Credential not found: {id}")).into()
}

pub(super) fn integration_not_found(id: &str) -> ApiError {
    ApiError::not_found(
        "IntegrationNotFoundError",
        format!("Integration not found: {id}"),
    )
    .with("integrationID", json!(id))
}

pub(super) fn attempt_not_found(integration_id: &str, attempt_id: &str) -> ApiError {
    ApiError::not_found(
        "IntegrationAttemptNotFoundError",
        format!("Integration attempt not found: {attempt_id}"),
    )
    .with("integrationID", json!(integration_id))
    .with("attemptID", json!(attempt_id))
}

pub(super) fn invalid_request(kind: &str, message: impl Into<String>) -> ApiError {
    ApiError::new(400, "InvalidRequestError", message).with("kind", json!(kind))
}

pub(super) fn not_implemented(message: &str) -> ApiError {
    ApiError::new(501, "NotImplementedError", message)
}
