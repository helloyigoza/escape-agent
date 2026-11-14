//! Antigravity Google OAuth 2.0 + PKCE helpers, URL generation, and token exchange.

use crate::auth::{RefreshParts, format_refresh_parts};
use crate::constants::{
    ANTIGRAVITY_REDIRECT_URI, ANTIGRAVITY_SCOPES, AntigravityEndpoints, oauth_client_id,
    oauth_client_secret,
};
use crate::error::{AntigravityError, Result};
use crate::project::ensure_project_context;
use crate::version::VersionTracker;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// PKCE verifier + challenge pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkcePair {
    /// High-entropy code verifier sent on token exchange.
    pub verifier: String,
    /// `S256` code challenge sent on authorization URL.
    pub challenge: String,
}

/// JSON payload encoded into the OAuth `state` parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthStatePayload {
    /// PKCE code verifier.
    pub verifier: String,
    /// Optional user-specified Google Cloud project ID.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project_id: String,
}

/// Authorization request details returned to the caller / UI / CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityAuthorization {
    /// Full Google OAuth 2.0 consent URL.
    pub url: String,
    /// PKCE verifier kept for validation and exchange.
    pub verifier: String,
    /// Base64url-encoded state string.
    pub state: String,
    /// Configured redirect URI used in the request.
    pub redirect_uri: String,
    /// Optional explicit project ID requested by the user.
    pub project_id: String,
}

/// Completed Antigravity OAuth credential result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityTokenExchange {
    /// Packed `refreshToken|projectId|managedProjectId`.
    pub refresh: String,
    /// Raw Google OAuth refresh token.
    pub refresh_token: String,
    /// Fresh OAuth access token.
    pub access: String,
    /// Expiration timestamp in epoch milliseconds.
    pub expires: i64,
    /// Authenticated Google account email, if returned by `userinfo`.
    pub email: Option<String>,
    /// Effective project ID associated with the credential.
    pub project_id: String,
    /// Explicit user project ID, if provided.
    pub explicit_project_id: Option<String>,
    /// Managed project ID provisioned by Cloud Code Assist, if discovered.
    pub managed_project_id: Option<String>,
}

/// Result of parsing a callback URL, query fragment, or bare authorization code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCallbackInput {
    /// Authorization code extracted from the input.
    pub code: String,
    /// Optional OAuth `state` parameter extracted from a callback URL.
    pub state: Option<String>,
}

/// Generate a cryptographic PKCE verifier and SHA-256 challenge.
#[must_use]
pub fn generate_pkce() -> PkcePair {
    let mut raw = [0u8; 32];
    rand::rng().fill_bytes(&mut raw);
    let verifier = URL_SAFE_NO_PAD.encode(raw);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(digest);
    PkcePair {
        verifier,
        challenge,
    }
}

/// Encode [`OAuthStatePayload`] as base64url JSON.
pub fn encode_oauth_state(payload: &OAuthStatePayload) -> Result<String> {
    let json = serde_json::to_vec(payload)?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

/// Decode base64url JSON back into [`OAuthStatePayload`].
pub fn decode_oauth_state(state: &str) -> Result<OAuthStatePayload> {
    let trimmed = state.trim();
    if trimmed.is_empty() {
        return Err(AntigravityError::OAuth("empty OAuth state".to_owned()));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(trimmed)
        .map_err(|err| AntigravityError::OAuth(format!("invalid OAuth state encoding: {err}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|err| AntigravityError::OAuth(format!("invalid OAuth state payload: {err}")))
}

/// Build an Antigravity Google OAuth 2.0 + PKCE authorization URL.
pub fn authorize_antigravity(
    project_id: Option<&str>,
    endpoints: &AntigravityEndpoints,
    redirect_uri: Option<&str>,
) -> Result<AntigravityAuthorization> {
    let pkce = generate_pkce();
    let project = project_id.map(str::trim).unwrap_or_default().to_owned();
    let state = encode_oauth_state(&OAuthStatePayload {
        verifier: pkce.verifier.clone(),
        project_id: project.clone(),
    })?;
    let effective_redirect = redirect_uri.unwrap_or(ANTIGRAVITY_REDIRECT_URI);

    let mut auth_url = url::Url::parse(&endpoints.oauth_auth_url)
        .map_err(|err| AntigravityError::OAuth(format!("invalid auth URL: {err}")))?;
    let scopes = ANTIGRAVITY_SCOPES.join(" ");
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", &oauth_client_id())
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", effective_redirect)
        .append_pair("scope", &scopes)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");

    Ok(AntigravityAuthorization {
        url: auth_url.to_string(),
        verifier: pkce.verifier,
        state,
        redirect_uri: effective_redirect.to_owned(),
        project_id: project,
    })
}

/// Parse user-pasted manual OAuth callback input (full callback URL, query string, or raw code).
pub fn parse_oauth_callback_input(raw: &str) -> Result<ParsedCallbackInput> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AntigravityError::OAuth(
            "OAuth authorization code or callback URL is empty".to_owned(),
        ));
    }

    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let parsed = url::Url::parse(trimmed)
            .map_err(|err| AntigravityError::OAuth(format!("invalid callback URL: {err}")))?;
        let mut code = None;
        let mut state = None;
        let mut error = None;
        for (k, v) in parsed.query_pairs() {
            match k.as_ref() {
                "code" if !v.is_empty() => code = Some(v.into_owned()),
                "state" if !v.is_empty() => state = Some(v.into_owned()),
                "error" if !v.is_empty() => error = Some(v.into_owned()),
                _ => {}
            }
        }
        if let Some(err) = error {
            return Err(AntigravityError::OAuth(format!(
                "Google OAuth returned error: {err}"
            )));
        }
        let code = code.ok_or_else(|| {
            AntigravityError::OAuth("callback URL does not contain `code` parameter".to_owned())
        })?;
        return Ok(ParsedCallbackInput { code, state });
    }

    if trimmed.contains("code=") {
        let synthetic = format!("http://localhost/?{}", trimmed.trim_start_matches('?'));
        if let Ok(parsed) = url::Url::parse(&synthetic) {
            let mut code = None;
            let mut state = None;
            for (k, v) in parsed.query_pairs() {
                match k.as_ref() {
                    "code" if !v.is_empty() => code = Some(v.into_owned()),
                    "state" if !v.is_empty() => state = Some(v.into_owned()),
                    _ => {}
                }
            }
            if let Some(code) = code {
                return Ok(ParsedCallbackInput { code, state });
            }
        }
    }

    // Bare authorization code pasted directly by the user.
    Ok(ParsedCallbackInput {
        code: trimmed.to_owned(),
        state: None,
    })
}

#[derive(Debug, Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    #[serde(default = "default_expires_in")]
    expires_in: i64,
    #[serde(default)]
    refresh_token: Option<String>,
}

const fn default_expires_in() -> i64 {
    3600
}

#[derive(Debug, Deserialize)]
struct UserInfoResponse {
    #[serde(default)]
    email: Option<String>,
}

/// Exchange an authorization `code` + encoded `state` for OAuth tokens, user email,
/// and managed Cloud Code Assist project context.
#[allow(clippy::too_many_arguments)]
pub async fn exchange_antigravity(
    client: &reqwest::Client,
    code: &str,
    state: &str,
    redirect_uri: Option<&str>,
    endpoints: &AntigravityEndpoints,
    version: &VersionTracker,
    now_ms: i64,
) -> Result<AntigravityTokenExchange> {
    let decoded = decode_oauth_state(state)?;
    let effective_redirect = redirect_uri.unwrap_or(ANTIGRAVITY_REDIRECT_URI);

    let params = [
        ("client_id", oauth_client_id()),
        ("client_secret", oauth_client_secret()),
        ("code", code.trim().to_owned()),
        ("grant_type", "authorization_code".to_owned()),
        ("redirect_uri", effective_redirect.to_owned()),
        ("code_verifier", decoded.verifier.clone()),
    ];

    let token_resp = client
        .post(&endpoints.oauth_token_url)
        .form(&params)
        .send()
        .await?;

    if !token_resp.status().is_success() {
        let status = token_resp.status();
        let body = token_resp.text().await.unwrap_or_default();
        return Err(AntigravityError::OAuth(format!(
            "token exchange failed (HTTP {status}): {body}"
        )));
    }

    let token_data: TokenExchangeResponse = token_resp.json().await?;
    let refresh_token = token_data
        .refresh_token
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            AntigravityError::OAuth(
                "missing refresh_token in Google OAuth response; ensure prompt=consent & access_type=offline".to_owned(),
            )
        })?;

    let email = fetch_userinfo_email(
        client,
        &endpoints.oauth_userinfo_url,
        &token_data.access_token,
    )
    .await;

    let explicit_project_id =
        (!decoded.project_id.trim().is_empty()).then(|| decoded.project_id.trim().to_owned());

    let ctx = ensure_project_context(
        client,
        endpoints,
        version,
        &token_data.access_token,
        explicit_project_id.as_deref(),
        true,
    )
    .await?;

    let expires = now_ms + token_data.expires_in.saturating_mul(1000);
    let packed = format_refresh_parts(&RefreshParts {
        refresh_token: refresh_token.clone(),
        project_id: explicit_project_id.clone(),
        managed_project_id: ctx.managed_project_id.clone(),
    });

    Ok(AntigravityTokenExchange {
        refresh: packed,
        refresh_token,
        access: token_data.access_token,
        expires,
        email,
        project_id: ctx.effective_project_id,
        explicit_project_id,
        managed_project_id: ctx.managed_project_id,
    })
}

/// Fetch the authenticated Google user's email address using an access token.
pub async fn fetch_userinfo_email(
    client: &reqwest::Client,
    userinfo_url: &str,
    access_token: &str,
) -> Option<String> {
    let resp = client
        .get(userinfo_url)
        .bearer_auth(access_token)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: UserInfoResponse = resp.json().await.ok()?;
    body.email
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_and_state_roundtrip() {
        let pkce = generate_pkce();
        assert!(!pkce.verifier.is_empty());
        assert!(!pkce.challenge.is_empty());
        assert_ne!(pkce.verifier, pkce.challenge);

        let encoded = encode_oauth_state(&OAuthStatePayload {
            verifier: pkce.verifier.clone(),
            project_id: "my-proj".to_owned(),
        })
        .expect("encode");
        let decoded = decode_oauth_state(&encoded).expect("decode");
        assert_eq!(decoded.verifier, pkce.verifier);
        assert_eq!(decoded.project_id, "my-proj");
    }

    #[test]
    fn parse_callback_supports_url_query_and_raw_code() {
        let full = parse_oauth_callback_input(
            "http://localhost:51121/oauth-callback?code=4/abc&state=xyz",
        )
        .expect("url");
        assert_eq!(full.code, "4/abc");
        assert_eq!(full.state.as_deref(), Some("xyz"));

        let query = parse_oauth_callback_input("code=4/def&state=uvw").expect("query");
        assert_eq!(query.code, "4/def");
        assert_eq!(query.state.as_deref(), Some("uvw"));

        let bare = parse_oauth_callback_input("4/bare_code").expect("bare");
        assert_eq!(bare.code, "4/bare_code");
        assert!(bare.state.is_none());
    }
}
