//! Error types for the `oc-antigravity` crate.

use thiserror::Error;

/// Result alias for operations in `oc-antigravity`.
pub type Result<T> = std::result::Result<T, AntigravityError>;

/// Errors produced by Antigravity OAuth, account management, request/response
/// transforms, and Cloud Code Assist gateway calls.
#[derive(Debug, Error)]
pub enum AntigravityError {
    /// Invalid or unreadable Antigravity configuration.
    #[error("antigravity config error: {0}")]
    Config(String),

    /// Account storage load/save/migration failure.
    #[error("antigravity storage error: {0}")]
    Storage(String),

    /// OAuth authorization or token exchange failure.
    #[error("antigravity oauth error: {0}")]
    OAuth(String),

    /// OAuth access-token refresh failure.
    #[error("antigravity token refresh failed (HTTP {status}): {message}")]
    TokenRefresh {
        /// HTTP status code returned by Google OAuth token endpoint.
        status: u16,
        /// Optional Google OAuth error code (e.g. `invalid_grant`).
        code: Option<String>,
        /// Human-readable error description.
        message: String,
        /// Whether the refresh token is permanently revoked.
        revoked: bool,
    },

    /// Cloud Code Assist project provisioning (`loadCodeAssist` / `onboardUser`) failure.
    #[error("antigravity project error: {0}")]
    Project(String),

    /// No enabled, usable Antigravity account is currently available.
    #[error("no available antigravity account: {0}")]
    NoAccounts(String),

    /// Account requires identity/account verification (`403 validation_required`).
    #[error("antigravity account requires verification: {message}")]
    ValidationRequired {
        /// Email of the account needing verification, if known.
        email: Option<String>,
        /// Validation URL returned by Google, if any.
        url: Option<String>,
        /// Upstream error detail.
        message: String,
    },

    /// Upstream rate limit / capacity exhaustion across all eligible accounts.
    #[error("antigravity rate limited (retry after {retry_after_ms}ms): {message}")]
    RateLimited {
        /// Suggested backoff duration in milliseconds.
        retry_after_ms: u64,
        /// Upstream error summary.
        message: String,
    },

    /// Non-retryable upstream Cloud Code Assist error.
    #[error("antigravity upstream error (HTTP {status}): {body}")]
    Upstream {
        /// HTTP status code.
        status: u16,
        /// Raw response body snippet.
        body: String,
    },

    /// Upstream returned a stream/response with zero content after retries.
    #[error("antigravity empty response: {0}")]
    EmptyResponse(String),

    /// Protocol or request/response transform failure.
    #[error("antigravity protocol error: {0}")]
    Protocol(String),

    /// HTTP transport error.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// Filesystem I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// SQLite credential store synchronization error.
    #[error("store error: {0}")]
    Store(#[from] oc_store::StoreError),
}
