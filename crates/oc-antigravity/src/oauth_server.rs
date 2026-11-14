//! Localhost loopback HTTP callback listener for the Antigravity OAuth flow.

use crate::constants::ANTIGRAVITY_OAUTH_PORT;
use crate::error::{AntigravityError, Result};
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, oneshot};

/// Result captured by [`LoopbackOAuthListener`] on `/oauth-callback`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthCallbackCapture {
    /// Authorization code delivered by Google OAuth redirect.
    pub code: String,
    /// `state` parameter returned by Google OAuth redirect.
    pub state: Option<String>,
}

type CallbackSender = oneshot::Sender<std::result::Result<OAuthCallbackCapture, String>>;
type CallbackReceiver = oneshot::Receiver<std::result::Result<OAuthCallbackCapture, String>>;

#[derive(Clone)]
struct CallbackAppState {
    expected_state: Option<String>,
    tx: Arc<Mutex<Option<CallbackSender>>>,
}

/// Detect whether the current process runs in a headless / SSH / remote shell without
/// a local browser reachable on `localhost:51121`.
#[must_use]
pub fn is_remote_environment() -> bool {
    const REMOTE_VARS: &[&str] = &[
        "SSH_CONNECTION",
        "SSH_CLIENT",
        "SSH_TTY",
        "WSL_DISTRO_NAME",
        "CODESPACES",
        "GITPOD_WORKSPACE_ID",
        "CLOUD_SHELL",
        "REMOTE_CONTAINERS",
        "DEVPOD",
    ];
    for var in REMOTE_VARS {
        if let Ok(val) = std::env::var(var)
            && !val.trim().is_empty()
        {
            return true;
        }
    }
    cfg!(target_os = "linux")
        && std::env::var("DISPLAY").is_err()
        && std::env::var("WAYLAND_DISPLAY").is_err()
}

/// Handle to a running localhost loopback callback server.
#[derive(Debug)]
pub struct LoopbackOAuthListener {
    addr: SocketAddr,
    rx: CallbackReceiver,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl LoopbackOAuthListener {
    /// Start a loopback OAuth listener on port `port` (default [`ANTIGRAVITY_OAUTH_PORT`], or `0` in tests).
    pub async fn bind(
        port: Option<u16>,
        bind_all: bool,
        expected_state: Option<String>,
    ) -> Result<Self> {
        let host = if bind_all {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        };
        let target_port = port.unwrap_or(ANTIGRAVITY_OAUTH_PORT);
        let listener = TcpListener::bind(SocketAddr::new(host, target_port))
            .await
            .map_err(|err| {
                AntigravityError::OAuth(format!(
                    "failed to bind OAuth callback listener on {host}:{target_port}: {err}"
                ))
            })?;
        let local_addr = listener.local_addr()?;

        let (result_tx, result_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let state = CallbackAppState {
            expected_state,
            tx: Arc::new(Mutex::new(Some(result_tx))),
        };

        let app = Router::new()
            .route("/oauth-callback", get(handle_callback))
            .with_state(state);

        tokio::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            });
            let _ = server.await;
        });

        Ok(Self {
            addr: local_addr,
            rx: result_rx,
            shutdown_tx: Some(shutdown_tx),
        })
    }

    /// Actual bound socket address.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Redirect URI matching this listener (`http://localhost:<port>/oauth-callback`).
    #[must_use]
    pub fn redirect_uri(&self) -> String {
        format!("http://localhost:{}/oauth-callback", self.addr.port())
    }

    /// Wait for the OAuth callback up to `timeout` or cancel cleanly.
    pub async fn wait_for_callback(
        mut self,
        timeout: std::time::Duration,
    ) -> Result<OAuthCallbackCapture> {
        let result = match tokio::time::timeout(timeout, &mut self.rx).await {
            Ok(Ok(Ok(capture))) => Ok(capture),
            Ok(Ok(Err(msg))) => Err(AntigravityError::OAuth(msg)),
            Ok(Err(_)) => Err(AntigravityError::OAuth(
                "OAuth callback listener closed before receiving callback".to_owned(),
            )),
            Err(_) => Err(AntigravityError::OAuth(
                "timed out waiting for Google OAuth callback".to_owned(),
            )),
        };
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        result
    }
}

impl Drop for LoopbackOAuthListener {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

async fn handle_callback(
    State(state): State<CallbackAppState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if let Some(err) = params.get("error") {
        let desc = params
            .get("error_description")
            .cloned()
            .unwrap_or_else(|| err.clone());
        if let Some(tx) = state.tx.lock().await.take() {
            let _ = tx.send(Err(format!("Google OAuth error ({err}): {desc}")));
        }
        return (
            StatusCode::BAD_REQUEST,
            Html(render_html(
                "Antigravity OAuth Failed",
                &format!("Authorization was denied or failed: {desc}"),
            )),
        );
    }

    let Some(code) = params.get("code").filter(|c| !c.trim().is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Html(render_html(
                "Missing Authorization Code",
                "The OAuth callback URL did not include a `code` parameter.",
            )),
        );
    };

    let got_state = params.get("state").cloned();
    if let Some(ref expected) = state.expected_state
        && got_state.as_deref() != Some(expected.as_str())
    {
        if let Some(tx) = state.tx.lock().await.take() {
            let _ = tx.send(Err("OAuth state mismatch (possible CSRF)".to_owned()));
        }
        return (
            StatusCode::BAD_REQUEST,
            Html(render_html(
                "OAuth State Mismatch",
                "State verification failed. Please restart authentication in OpenCode.",
            )),
        );
    }

    if let Some(tx) = state.tx.lock().await.take() {
        let _ = tx.send(Ok(OAuthCallbackCapture {
            code: code.clone(),
            state: got_state,
        }));
    }

    (
        StatusCode::OK,
        Html(render_html(
            "Antigravity Authentication Complete",
            "You are signed in! You can close this window and return to OpenCode.",
        )),
    )
}

/// Escape the five HTML-significant characters so untrusted OAuth error text can never
/// break out of the `<title>` / `<p>` element it is rendered into.
#[must_use]
pub fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

fn render_html(title: &str, body: &str) -> String {
    let title = escape_html(title);
    let body = escape_html(body);
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title>\
         <style>body{{font-family:system-ui,-apple-system,sans-serif;max-width:520px;margin:64px auto;padding:0 24px;color:#111}}\
         h1{{font-size:20px;margin-bottom:12px}}p{{color:#444;line-height:1.5}}</style></head>\
         <body><h1>{title}</h1><p>{body}</p></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn loopback_listener_captures_code_and_validates_state() {
        let listener = LoopbackOAuthListener::bind(Some(0), false, Some("st123".to_owned()))
            .await
            .expect("bind");
        let port = listener.local_addr().port();

        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/oauth-callback?code=4/testcode&state=st123");
        let resp = client.get(&url).send().await.expect("get callback");
        assert_eq!(resp.status(), StatusCode::OK);

        let capture = listener
            .wait_for_callback(Duration::from_secs(2))
            .await
            .expect("capture");
        assert_eq!(capture.code, "4/testcode");
        assert_eq!(capture.state.as_deref(), Some("st123"));
    }

    #[tokio::test]
    async fn callback_error_description_is_html_escaped() {
        let listener = LoopbackOAuthListener::bind(Some(0), false, None)
            .await
            .expect("bind");
        let port = listener.local_addr().port();

        let payload = "<script>alert(1)</script>";
        let url = format!(
            "http://127.0.0.1:{port}/oauth-callback?error=access_denied&error_description={}",
            percent_encoding::utf8_percent_encode(payload, percent_encoding::NON_ALPHANUMERIC)
        );
        let resp = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .expect("get callback");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = resp.text().await.expect("body");

        assert!(
            !body.contains("<script>"),
            "reflected script tag leaked into callback page: {body}"
        );
        assert!(
            body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "error_description was not escaped: {body}"
        );

        // The listener itself still reports the raw text to the OAuth caller.
        let err = listener
            .wait_for_callback(Duration::from_secs(2))
            .await
            .expect_err("error callback");
        assert!(err.to_string().contains(payload));
    }

    #[test]
    fn escape_html_covers_the_five_significant_characters() {
        assert_eq!(escape_html("&<>\"'"), "&amp;&lt;&gt;&quot;&#39;".to_owned());
        assert_eq!(escape_html("plain text 42"), "plain text 42");
    }
}
