//! Integration and credential routes.
//!
//! Upstream keeps a registry of integrations that plugins and the MCP manager
//! populate, plus a credential table. This build has no plugin host attached, so
//! the registry is derived from what the process really knows:
//!
//! - every provider the catalog knows (skipping `Disabled` ones), merged with
//!   the configured `providers` map, which overrides the name and environment
//!   variables; each row has the `key` method upstream registers ("Manually
//!   enter API Key") and an `env` method when the effective env list is not
//!   empty;
//! - every remote MCP server whose OAuth is not disabled, with one `oauth`
//!   method (id and integration id follow upstream: `mcp_` plus a hash of
//!   `name\0url`);
//! - integrations that exist only because a stored credential references them,
//!   listed without methods so no capability is invented.
//!
//! Connections are the credentials stored for an integration (newest first)
//! plus the environment variables of its `env` methods that are actually set.
//!
//! Credentials are persisted through `oc-store`; optional wire fields are
//! omitted, never sent as `null`. Updating, activating or removing an unknown
//! credential is the upstream no-op and answers `204`, while the extra
//! `GET /api/credential/{id}` (not in the `OpenAPI` document) answers `404`
//! `NotFoundError` for a missing row.
//!
//! OAuth, external and command authorization need engines or method
//! registrations this build does not attach: their routes exist and validate
//! their input, then answer the documented `400 InvalidRequestError` for a
//! method that is not registered and `501 NotImplementedError` for a method that
//! is registered but has no engine behind it. Attempt status routes answer the
//! documented `404 IntegrationAttemptNotFoundError` because no attempt can
//! exist. Well-known discovery answers `503` because discovery is not
//! configured.

mod bodies;
mod command;
mod connect;
mod credential;
mod errors;
mod registry;
mod view;

use axum::Router;
use axum::routing::{get, post};

use crate::AppState;

pub use bodies::{
    CommandConnectBody, CreateCredentialBody, KeyConnectBody, MethodConnectBody, OAuthCompleteBody,
    UpdateCredentialBody, WellKnownBody,
};
pub use command::{
    command_cancel, command_connect, command_status, wellknown_add, wellknown_status,
};
pub use connect::{
    connect_external, connect_key, connect_oauth, oauth_cancel, oauth_complete, oauth_status,
};
pub use credential::{
    credential_activate, credential_create, credential_get, credential_list, credential_remove,
    credential_update,
};
pub use view::{integration_get, integration_list};

/// Credential and integration routes, merged into the protected router.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/credential",
            get(credential_list).post(credential_create),
        )
        .route(
            "/api/credential/{id}",
            get(credential_get)
                .patch(credential_update)
                .delete(credential_remove),
        )
        .route("/api/credential/{id}/activate", post(credential_activate))
        .route("/api/integration", get(integration_list))
        .route("/api/integration/{id}", get(integration_get))
        .route("/api/integration/{id}/connect/key", post(connect_key))
        .route(
            "/api/integration/{id}/connect/external",
            post(connect_external),
        )
        .route("/api/integration/{id}/connect/oauth", post(connect_oauth))
        .route(
            "/api/integration/{id}/connect/oauth/{attempt_id}",
            get(oauth_status).delete(oauth_cancel),
        )
        .route(
            "/api/integration/{id}/connect/oauth/{attempt_id}/complete",
            post(oauth_complete),
        )
        .route(
            "/api/integration/{id}/connect/command",
            post(command_connect),
        )
        .route(
            "/api/integration/{id}/connect/command/{attempt_id}",
            get(command_status).delete(command_cancel),
        )
        .route(
            "/api/experimental/integration/wellknown",
            get(wellknown_status).post(wellknown_add),
        )
}
