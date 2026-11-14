//! Refresh-token packing (`refreshToken|projectId|managedProjectId`) and OAuth auth state helpers.

use crate::constants::ANTIGRAVITY_PROVIDER_ID;
use serde::{Deserialize, Serialize};

/// Safety buffer (seconds) before expiry at which a token is treated as expired.
pub const ACCESS_TOKEN_EXPIRY_BUFFER_MS: i64 = 60_000;

/// Packed parts parsed from an Antigravity `refresh` string.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RefreshParts {
    /// Raw Google OAuth 2.0 refresh token.
    pub refresh_token: String,
    /// Optional user-configured Google Cloud project ID.
    pub project_id: Option<String>,
    /// Managed project ID discovered/onboarded via `loadCodeAssist` / `onboardUser`.
    pub managed_project_id: Option<String>,
}

/// Split a packed refresh string (`refreshToken|projectId|managedProjectId`).
#[must_use]
pub fn parse_refresh_parts(refresh: &str) -> RefreshParts {
    let mut iter = refresh.splitn(3, '|');
    let refresh_token = iter.next().unwrap_or_default().to_owned();
    let project_id = iter
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let managed_project_id = iter
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    RefreshParts {
        refresh_token,
        project_id,
        managed_project_id,
    }
}

/// Format [`RefreshParts`] back into `refreshToken|projectId|managedProjectId`.
#[must_use]
pub fn format_refresh_parts(parts: &RefreshParts) -> String {
    let project = parts.project_id.as_deref().unwrap_or("");
    let managed = parts.managed_project_id.as_deref();
    if let Some(m) = managed
        && !m.is_empty()
    {
        return format!("{}|{}|{}", parts.refresh_token, project, m);
    }
    if !project.is_empty() {
        return format!("{}|{}", parts.refresh_token, project);
    }
    parts.refresh_token.clone()
}

/// Current OAuth credential view compatible with `oc-store` (`type = "oauth"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthAuthDetails {
    /// Packed `refreshToken|projectId|managedProjectId`.
    pub refresh: String,
    /// Current OAuth access token (may be empty if refresh is needed).
    pub access: Option<String>,
    /// Epoch millisecond expiration timestamp.
    pub expires: Option<i64>,
}

/// Check whether an OAuth access token is missing or within the expiry buffer at `now_ms`.
#[must_use]
pub fn access_token_expired(auth: &OAuthAuthDetails, now_ms: i64) -> bool {
    let Some(ref access) = auth.access else {
        return true;
    };
    if access.trim().is_empty() {
        return true;
    }
    let Some(expires) = auth.expires else {
        return true;
    };
    expires <= now_ms + ACCESS_TOKEN_EXPIRY_BUFFER_MS
}

/// Return `true` when `provider_id` is Google / Antigravity (`"google"`).
#[must_use]
pub fn is_antigravity_provider(provider_id: &str) -> bool {
    provider_id.eq_ignore_ascii_case(ANTIGRAVITY_PROVIDER_ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_refresh_parts() {
        let p1 = RefreshParts {
            refresh_token: "1//abc".to_owned(),
            project_id: None,
            managed_project_id: None,
        };
        assert_eq!(format_refresh_parts(&p1), "1//abc");
        assert_eq!(parse_refresh_parts("1//abc"), p1);

        let p2 = RefreshParts {
            refresh_token: "1//abc".to_owned(),
            project_id: Some("proj-1".to_owned()),
            managed_project_id: None,
        };
        assert_eq!(format_refresh_parts(&p2), "1//abc|proj-1");
        assert_eq!(parse_refresh_parts("1//abc|proj-1"), p2);

        let p3 = RefreshParts {
            refresh_token: "1//abc".to_owned(),
            project_id: None,
            managed_project_id: Some("managed-9".to_owned()),
        };
        assert_eq!(format_refresh_parts(&p3), "1//abc||managed-9");
        assert_eq!(parse_refresh_parts("1//abc||managed-9"), p3);
    }

    #[test]
    fn expiry_buffer_check() {
        let auth = OAuthAuthDetails {
            refresh: "r".to_owned(),
            access: Some("ya29.token".to_owned()),
            expires: Some(1_060_000),
        };
        assert!(!access_token_expired(&auth, 999_999));
        assert!(access_token_expired(&auth, 1_000_000));
    }
}
