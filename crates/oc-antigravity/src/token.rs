//! Single-flight OAuth access-token refresh cache with rotation and revocation detection.

use crate::auth::{OAuthAuthDetails, format_refresh_parts, parse_refresh_parts};
use crate::constants::{oauth_client_id, oauth_client_secret};
use crate::error::{AntigravityError, Result};
use serde::Deserialize;
use std::collections::HashMap;
use tokio::sync::{Mutex, watch};

/// Result of a token refresh operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshedAccessToken {
    /// Fresh OAuth access token.
    pub access: String,
    /// Expiration timestamp in epoch milliseconds.
    pub expires: i64,
    /// Updated packed refresh string if Google rotated the refresh token.
    pub rotated_refresh: Option<String>,
    /// Raw refresh token after any rotation.
    pub refresh_token: String,
}

#[derive(Debug, Clone)]
struct RefreshErrorSnapshot {
    status: u16,
    code: Option<String>,
    message: String,
    revoked: bool,
}

impl From<&AntigravityError> for RefreshErrorSnapshot {
    fn from(err: &AntigravityError) -> Self {
        match err {
            AntigravityError::TokenRefresh {
                status,
                code,
                message,
                revoked,
            } => Self {
                status: *status,
                code: code.clone(),
                message: message.clone(),
                revoked: *revoked,
            },
            other => Self {
                status: 500,
                code: None,
                message: other.to_string(),
                revoked: false,
            },
        }
    }
}

type FlightResult = std::result::Result<RefreshedAccessToken, RefreshErrorSnapshot>;

/// Single-flight deduplicator for concurrent Google OAuth refresh requests per refresh token.
#[derive(Debug, Default)]
pub struct TokenRefreshCoordinator {
    inflight: Mutex<HashMap<String, watch::Receiver<Option<FlightResult>>>>,
}

#[derive(Debug, Deserialize)]
struct GoogleRefreshResponse {
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
struct GoogleOAuthErrorBody {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

impl TokenRefreshCoordinator {
    /// Create an empty token refresh coordinator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Refresh the access token for `auth`, coalescing concurrent callers sharing the same refresh token.
    pub async fn refresh_access_token(
        &self,
        client: &reqwest::Client,
        token_url: &str,
        auth: &OAuthAuthDetails,
        now_ms: i64,
    ) -> Result<RefreshedAccessToken> {
        let parts = parse_refresh_parts(&auth.refresh);
        if parts.refresh_token.trim().is_empty() {
            return Err(AntigravityError::TokenRefresh {
                status: 400,
                code: Some("missing_refresh_token".to_owned()),
                message: "refresh token is empty".to_owned(),
                revoked: true,
            });
        }

        let key = parts.refresh_token.clone();

        let leader_tx = {
            let mut map = self.inflight.lock().await;
            if let Some(existing_rx) = map.get(&key).cloned() {
                drop(map);
                let mut rx = existing_rx;
                loop {
                    if let Some(res) = rx.borrow_and_update().clone() {
                        return unwrap_flight_result(res);
                    }
                    if rx.changed().await.is_err() {
                        return Err(AntigravityError::TokenRefresh {
                            status: 500,
                            code: None,
                            message: "in-flight token refresh aborted".to_owned(),
                            revoked: false,
                        });
                    }
                }
            }
            let (tx, rx) = watch::channel(None);
            map.insert(key.clone(), rx);
            tx
        };

        let result = perform_refresh(client, token_url, &parts, &auth.refresh, now_ms).await;
        let snapshot = result.as_ref().map_err(RefreshErrorSnapshot::from).cloned();

        // Remove entry first or after sending? Waiters already hold `rx` cloned from the channel,
        // and `send` delivers `Some(snapshot)` to all existing `rx` receivers even after map removal.
        {
            let mut map = self.inflight.lock().await;
            map.remove(&key);
        }
        let _ = leader_tx.send(Some(snapshot));

        result
    }
}

fn unwrap_flight_result(res: FlightResult) -> Result<RefreshedAccessToken> {
    match res {
        Ok(ok) => Ok(ok),
        Err(snap) => Err(AntigravityError::TokenRefresh {
            status: snap.status,
            code: snap.code,
            message: snap.message,
            revoked: snap.revoked,
        }),
    }
}

async fn perform_refresh(
    client: &reqwest::Client,
    token_url: &str,
    parts: &crate::auth::RefreshParts,
    original_packed: &str,
    now_ms: i64,
) -> Result<RefreshedAccessToken> {
    let params = [
        ("grant_type", "refresh_token".to_owned()),
        ("refresh_token", parts.refresh_token.clone()),
        ("client_id", oauth_client_id()),
        ("client_secret", oauth_client_secret()),
    ];

    let resp = client.post(token_url).form(&params).send().await?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let raw_body = resp.text().await.unwrap_or_default();
        let parsed: Option<GoogleOAuthErrorBody> = serde_json::from_str(&raw_body).ok();
        let code = parsed.as_ref().and_then(|b| b.error.clone());
        let message = parsed
            .as_ref()
            .and_then(|b| b.error_description.clone())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| raw_body.clone());
        let revoked =
            code.as_deref() == Some("invalid_grant") || raw_body.contains("invalid_grant");
        return Err(AntigravityError::TokenRefresh {
            status,
            code,
            message,
            revoked,
        });
    }

    let payload: GoogleRefreshResponse = resp.json().await?;
    let expires = now_ms + payload.expires_in.saturating_mul(1000);

    let (rotated_refresh, effective_raw) = match payload
        .refresh_token
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty() && *s != parts.refresh_token)
    {
        Some(new_raw) => {
            let mut updated_parts = parts.clone();
            updated_parts.refresh_token.clone_from(&new_raw);
            let packed = format_refresh_parts(&updated_parts);
            let changed = (packed != original_packed).then_some(packed);
            (changed, new_raw)
        }
        None => (None, parts.refresh_token.clone()),
    };

    Ok(RefreshedAccessToken {
        access: payload.access_token,
        expires,
        rotated_refresh,
        refresh_token: effective_raw,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn single_flight_coalesces_concurrent_refreshes() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/token",
                post(|State(count): State<Arc<AtomicUsize>>| async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "access_token": "ya29.fresh",
                            "expires_in": 3600,
                            "refresh_token": "1//rotated"
                        })),
                    )
                }),
            )
            .with_state(calls.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let coord = Arc::new(TokenRefreshCoordinator::new());
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/token");
        let auth = OAuthAuthDetails {
            refresh: "1//orig|proj-1".to_owned(),
            access: None,
            expires: None,
        };

        let (r1, r2, r3) = tokio::join!(
            coord.refresh_access_token(&client, &url, &auth, 1000),
            coord.refresh_access_token(&client, &url, &auth, 1000),
            coord.refresh_access_token(&client, &url, &auth, 1000),
        );

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for res in [r1, r2, r3] {
            let ok = res.expect("refresh ok");
            assert_eq!(ok.access, "ya29.fresh");
            assert_eq!(ok.rotated_refresh.as_deref(), Some("1//rotated|proj-1"));
        }
    }
}
