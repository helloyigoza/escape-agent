//! Wire request bodies for credential and integration endpoints.

use serde::Deserialize;
use serde_json::{Map, Value};

/// `POST /api/credential`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateCredentialBody {
    pub(super) id: Option<String>,
    #[serde(rename = "integrationID")]
    pub(super) integration_id: String,
    pub(super) label: Option<String>,
    pub(super) value: Value,
    pub(super) activate: Option<bool>,
}

/// `PATCH /api/credential/{id}`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateCredentialBody {
    pub(super) label: String,
}

/// `POST /api/integration/{id}/connect/key`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyConnectBody {
    pub(super) key: String,
    #[serde(default)]
    pub(super) answer: Option<Map<String, Value>>,
    pub(super) label: Option<String>,
}

/// `POST /api/integration/{id}/connect/external` and `/connect/oauth`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MethodConnectBody {
    #[serde(rename = "methodID")]
    pub(super) method_id: String,
    #[serde(default)]
    pub(super) answer: Option<Map<String, Value>>,
    pub(super) label: Option<String>,
}

/// `POST /api/integration/{id}/connect/oauth/{attemptID}/complete`
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthCompleteBody {
    #[serde(default)]
    pub(super) code: Option<String>,
}

/// `POST /api/integration/{id}/connect/command`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandConnectBody {
    #[serde(rename = "methodID")]
    pub(super) method_id: String,
    /// Accepted by the schema; an attempt cannot be started, so it is unused.
    #[serde(rename = "label")]
    pub(super) _label: Option<String>,
}

/// `POST /api/experimental/integration/wellknown`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WellKnownBody {
    pub(super) url: String,
}
