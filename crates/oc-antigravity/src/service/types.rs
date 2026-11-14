//! Public wire views and shared caches for [`AntigravityService`](super::AntigravityService).

use crate::storage::{AccountMetadataV4, CachedAccountQuota};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Public status summary for a single Google account in the Antigravity pool.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatusSummary {
    /// Zero-based pool index.
    pub index: usize,
    /// Stable short SHA-256 token hash identifier.
    pub id: String,
    /// Google account email, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Optional user label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// True if currently selected as the sticky active cursor for Claude.
    pub active_claude: bool,
    /// True if currently selected as the sticky active cursor for Gemini.
    pub active_gemini: bool,
    /// Whether account is enabled for routing.
    pub enabled: bool,
    /// Whether Google flagged `403 validation_required` on this account.
    pub verification_required: bool,
    /// Optional validation URL provided by Google.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_url: Option<String>,
    /// Whether refresh token is permanently revoked (`invalid_grant`).
    pub revoked: bool,
    /// Effective project ID (`projectId` or `managedProjectId`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Account subscription tier if cached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Active cooldown deadlines keyed by quota key (`claude`, `gemini-antigravity`, ...).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub rate_limits: HashMap<String, i64>,
    /// Cached quota usage snapshot across model groups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<CachedAccountQuota>,
    /// Timestamp added (epoch ms).
    pub added_at: i64,
    /// Timestamp last used (epoch ms).
    pub last_used: i64,
}

/// Aggregated response from refreshing live quotas across accounts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaRefreshReport {
    /// Updated timestamp (epoch ms).
    pub updated_at: i64,
    /// Updated account summaries.
    pub accounts: Vec<AccountStatusSummary>,
    /// Number of upstream models discovered and cached.
    pub discovered_model_count: usize,
    /// Non-fatal per-account warnings keyed by account hash/email.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// View matching `OpenAPI` `Integration.Attempt` for OAuth start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthAttemptView {
    /// Prefixed `att_...` attempt identifier.
    #[serde(rename = "attemptID")]
    pub attempt_id: String,
    /// Consent URL to open in a browser.
    pub url: String,
    /// Human-readable instructions matching current mode (`auto` vs `code`).
    pub instructions: String,
    /// `"auto"` when localhost loopback listener is running, `"code"` otherwise.
    pub mode: String,
    /// Created + expiration epoch ms timestamps.
    pub time: OAuthAttemptTime,
}

/// Created/expiration timestamp pair in `OpenAPI` `Integration.Attempt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthAttemptTime {
    /// Creation timestamp (epoch ms).
    pub created: i64,
    /// Expiration timestamp (epoch ms).
    pub expires: i64,
}

/// View matching `OpenAPI` `Integration.AttemptStatus`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum OAuthAttemptStatusView {
    /// Waiting for loopback callback or manual code submission.
    Pending {
        /// Attempt lifetime timestamps.
        time: OAuthAttemptTime,
    },
    /// OAuth exchange & project onboarding succeeded.
    Complete {
        /// Attempt lifetime timestamps.
        time: OAuthAttemptTime,
    },
    /// OAuth exchange failed or was rejected.
    Failed {
        /// Failure detail.
        message: String,
        /// Attempt lifetime timestamps.
        time: OAuthAttemptTime,
    },
}

/// Validated access token + effective Cloud Code project context for one request attempt.
#[derive(Debug, Clone)]
pub struct ActiveCredentials {
    /// Pool index (`usize::MAX` if synthetic fallback).
    pub account_index: usize,
    /// Account hash key.
    pub account_hash: String,
    /// Fresh OAuth access token.
    pub access_token: String,
    /// Effective Cloud Code project ID.
    pub project_id: String,
    /// Associated account metadata snapshot.
    pub account: AccountMetadataV4,
}

#[derive(Debug, Clone)]
pub(super) struct CachedAccessToken {
    pub(super) access: String,
    pub(super) expires: i64,
}
