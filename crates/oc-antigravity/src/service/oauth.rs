//! Google Antigravity OAuth attempts: start, loopback callback, poll, cancel, complete.

use super::rotation::short_random_hex;
use super::sync::{build_account_summary, lock_accounts};
use super::types::{
    AccountStatusSummary, CachedAccessToken, OAuthAttemptStatusView, OAuthAttemptTime,
    OAuthAttemptView,
};
use super::{ATTEMPT_TTL_MS, AntigravityService, now_ms};
use crate::auth::parse_refresh_parts;
use crate::error::{AntigravityError, Result};
use crate::fingerprint::generate_fingerprint;
use crate::oauth::{
    AntigravityAuthorization, AntigravityTokenExchange, authorize_antigravity,
    exchange_antigravity, parse_oauth_callback_input,
};
use crate::oauth_server::{LoopbackOAuthListener, is_remote_environment};
use crate::storage::{AccountMetadataV4, account_token_hash};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone)]
enum AttemptState {
    Pending,
    Complete,
    Failed(String),
}

#[derive(Debug, Clone)]
pub(super) struct OAuthAttemptEntry {
    auth: AntigravityAuthorization,
    label: Option<String>,
    created: i64,
    expires: i64,
    state: AttemptState,
    abort_tx: Option<tokio::sync::watch::Sender<bool>>,
}

impl AntigravityService {
    /// Begin a new Google Antigravity OAuth attempt (HTTP `/api/integration/google/connect/oauth`).
    ///
    /// When `use_loopback` is `true` and not in a remote headless environment, starts a local
    /// callback listener on `127.0.0.1:51121` (unless occupied, in which case `"code"` manual mode
    /// is returned transparently).
    pub async fn start_oauth(
        &self,
        project_id: Option<String>,
        label: Option<String>,
        use_loopback: bool,
        loopback_port: Option<u16>,
    ) -> Result<OAuthAttemptView> {
        let now = now_ms();
        let expires = now + ATTEMPT_TTL_MS;
        let attempt_id = format!("att_{}", short_random_hex());
        let (auth, listener) = self
            .prepare_oauth_authorization(project_id.as_deref(), use_loopback, loopback_port)
            .await?;

        let mode = if listener.is_some() { "auto" } else { "code" }.to_owned();
        let instructions = if mode == "auto" {
            format!(
                "Open the URL in your browser to sign in with Google (listening on {}).",
                auth.redirect_uri
            )
        } else {
            "Open the URL in your browser, approve access, then paste the final redirect URL or authorization code."
                .to_owned()
        };

        let (abort_tx, abort_rx) = tokio::sync::watch::channel(false);

        {
            let mut attempts = self.inner.oauth_attempts.lock().await;
            attempts.retain(|_, v| v.expires > now - ATTEMPT_TTL_MS);
            attempts.insert(
                attempt_id.clone(),
                OAuthAttemptEntry {
                    auth: auth.clone(),
                    label,
                    created: now,
                    expires,
                    state: AttemptState::Pending,
                    abort_tx: Some(abort_tx),
                },
            );
        }

        if let Some(bound_listener) = listener {
            self.spawn_loopback_waiter(attempt_id.clone(), auth.clone(), bound_listener, abort_rx);
        }

        Ok(OAuthAttemptView {
            attempt_id,
            url: auth.url,
            instructions,
            mode,
            time: OAuthAttemptTime {
                created: now,
                expires,
            },
        })
    }

    async fn prepare_oauth_authorization(
        &self,
        project_id: Option<&str>,
        use_loopback: bool,
        loopback_port: Option<u16>,
    ) -> Result<(AntigravityAuthorization, Option<LoopbackOAuthListener>)> {
        if !use_loopback || is_remote_environment() {
            let auth = authorize_antigravity(project_id, &self.inner.endpoints, None)?;
            return Ok((auth, None));
        }

        let bind_all = self.config().oauth_server_bind_all;
        let preliminary = authorize_antigravity(project_id, &self.inner.endpoints, None)?;
        let Ok(bound) =
            LoopbackOAuthListener::bind(loopback_port, bind_all, Some(preliminary.state.clone()))
                .await
        else {
            return Ok((preliminary, None));
        };

        let redir = bound.redirect_uri();
        if loopback_port.is_none()
            || loopback_port == Some(crate::constants::ANTIGRAVITY_OAUTH_PORT)
        {
            return Ok((preliminary, Some(bound)));
        }

        drop(bound);
        let rebuild_preview =
            authorize_antigravity(project_id, &self.inner.endpoints, Some(&redir))?;
        let Ok(rebound) = LoopbackOAuthListener::bind(
            loopback_port,
            bind_all,
            Some(rebuild_preview.state.clone()),
        )
        .await
        else {
            return Ok((preliminary, None));
        };
        let r2 = rebound.redirect_uri();
        let rebuilt = authorize_antigravity(project_id, &self.inner.endpoints, Some(&r2))?;
        Ok((rebuilt, Some(rebound)))
    }

    fn spawn_loopback_waiter(
        &self,
        attempt_id: String,
        auth: AntigravityAuthorization,
        bound_listener: LoopbackOAuthListener,
        mut abort_rx: tokio::sync::watch::Receiver<bool>,
    ) {
        let svc = self.clone();
        tokio::spawn(async move {
            let wait_fut = bound_listener.wait_for_callback(Duration::from_secs(600));
            tokio::select! {
                _ = abort_rx.changed() => {}
                cap = wait_fut => {
                    let state = match cap {
                        Ok(capture) => {
                            let state_str = capture.state.unwrap_or_else(|| auth.state.clone());
                            match exchange_antigravity(
                                &svc.inner.http,
                                &capture.code,
                                &state_str,
                                Some(&auth.redirect_uri),
                                &svc.inner.endpoints,
                                &svc.inner.version,
                                now_ms(),
                            )
                            .await
                            {
                                Ok(ex) => {
                                    let label = svc.get_attempt_label(&attempt_id).await;
                                    match svc.upsert_exchange_result(ex, label).await {
                                        Ok(_) => {
                                            let _ = svc.refresh_quotas_and_models().await;
                                            AttemptState::Complete
                                        }
                                        Err(e) => AttemptState::Failed(e.to_string()),
                                    }
                                }
                                Err(err) => AttemptState::Failed(err.to_string()),
                            }
                        }
                        Err(err) => AttemptState::Failed(err.to_string()),
                    };
                    svc.set_attempt_state(&attempt_id, state).await;
                }
            }
        });
    }

    /// Poll an OAuth attempt status (HTTP `GET /api/integration/google/connect/oauth/{attemptID}`).
    pub async fn poll_oauth(&self, attempt_id: &str) -> Option<OAuthAttemptStatusView> {
        let mut attempts = self.inner.oauth_attempts.lock().await;
        let entry = attempts.get_mut(attempt_id)?;
        let now = now_ms();
        if matches!(entry.state, AttemptState::Pending) && now > entry.expires {
            entry.state = AttemptState::Failed("OAuth attempt expired".to_owned());
        }
        let time = OAuthAttemptTime {
            created: entry.created,
            expires: entry.expires,
        };
        Some(match &entry.state {
            AttemptState::Pending => OAuthAttemptStatusView::Pending { time },
            AttemptState::Complete => OAuthAttemptStatusView::Complete { time },
            AttemptState::Failed(msg) => OAuthAttemptStatusView::Failed {
                message: msg.clone(),
                time,
            },
        })
    }

    /// Cancel an in-flight OAuth attempt (HTTP `DELETE /api/integration/google/connect/oauth/{attemptID}`).
    pub async fn cancel_oauth(&self, attempt_id: &str) -> bool {
        let mut attempts = self.inner.oauth_attempts.lock().await;
        if let Some(entry) = attempts.remove(attempt_id) {
            if let Some(tx) = entry.abort_tx {
                let _ = tx.send(true);
            }
            true
        } else {
            false
        }
    }

    /// Complete an OAuth attempt manually from a callback URL or authorization code.
    pub async fn complete_oauth(
        &self,
        attempt_id: &str,
        raw_input: Option<&str>,
    ) -> Result<AccountStatusSummary> {
        let (auth, label, existing_complete) = {
            let attempts = self.inner.oauth_attempts.lock().await;
            let entry = attempts.get(attempt_id).ok_or_else(|| {
                AntigravityError::OAuth(format!("OAuth attempt `{attempt_id}` not found"))
            })?;
            let is_complete = matches!(entry.state, AttemptState::Complete);
            (entry.auth.clone(), entry.label.clone(), is_complete)
        };

        if existing_complete && raw_input.is_none_or(|s| s.trim().is_empty()) {
            let accounts = self.list_accounts_status()?;
            return accounts.into_iter().next().ok_or_else(|| {
                AntigravityError::OAuth("OAuth completed but no account was stored".to_owned())
            });
        }

        let code_input = raw_input
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AntigravityError::OAuth(
                    "authorization code or callback URL is required to complete OAuth".to_owned(),
                )
            })?;

        let parsed = parse_oauth_callback_input(code_input)?;
        let effective_state = parsed.state.as_deref().unwrap_or(&auth.state);

        let exchange = match exchange_antigravity(
            &self.inner.http,
            &parsed.code,
            effective_state,
            Some(&auth.redirect_uri),
            &self.inner.endpoints,
            &self.inner.version,
            now_ms(),
        )
        .await
        {
            Ok(ex) => ex,
            Err(err) => {
                self.set_attempt_state(attempt_id, AttemptState::Failed(err.to_string()))
                    .await;
                return Err(err);
            }
        };

        let summary = self.upsert_exchange_result(exchange, label).await?;
        let _ = self.refresh_quotas_and_models().await;
        self.set_attempt_state(attempt_id, AttemptState::Complete)
            .await;
        Ok(summary)
    }

    /// Persist a completed OAuth token exchange into `AccountManager`, token cache, and `oc-store`.
    pub async fn upsert_exchange_result(
        &self,
        exchange: AntigravityTokenExchange,
        label: Option<String>,
    ) -> Result<AccountStatusSummary> {
        let now = now_ms();
        let parts = parse_refresh_parts(&exchange.refresh);
        let token_hash = account_token_hash(&parts.refresh_token);

        if let Ok(mut tc) = self.inner.token_cache.lock() {
            tc.insert(
                token_hash.clone(),
                CachedAccessToken {
                    access: exchange.access.clone(),
                    expires: exchange.expires,
                },
            );
        }

        // Persist/update credential in SQLite `oc_store::Store` first so `store_credential_id` stays linked.
        let store_cred_id = self
            .sync_credential_to_store(&exchange, label.clone())
            .await?;

        let summary = {
            let mut mgr = lock_accounts(&self.inner.accounts)?;
            let acct = AccountMetadataV4 {
                email: exchange.email.clone(),
                label,
                refresh_token: parts.refresh_token,
                project_id: parts.project_id,
                managed_project_id: parts.managed_project_id,
                added_at: now,
                last_used: now,
                enabled: true,
                rate_limit_reset_times: HashMap::new(),
                cached_tier: None,
                cached_quota: None,
                verification_required: false,
                verification_required_at: None,
                verification_required_reason: None,
                verification_url: None,
                revoked: false,
                fingerprint: Some(generate_fingerprint(now, &self.inner.version.current())),
                fingerprint_history: Vec::new(),
                token_hash,
                store_credential_id: store_cred_id,
            };
            let idx = mgr.upsert_account(acct, true)?;
            build_account_summary(&mgr, idx, now).ok_or_else(|| {
                AntigravityError::Storage("failed to build account summary".to_owned())
            })?
        };

        Ok(summary)
    }

    async fn get_attempt_label(&self, attempt_id: &str) -> Option<String> {
        let attempts = self.inner.oauth_attempts.lock().await;
        attempts.get(attempt_id).and_then(|e| e.label.clone())
    }

    async fn set_attempt_state(&self, attempt_id: &str, state: AttemptState) {
        let mut attempts = self.inner.oauth_attempts.lock().await;
        if let Some(entry) = attempts.get_mut(attempt_id) {
            entry.state = state;
            if let Some(tx) = entry.abort_tx.take() {
                let _ = tx.send(true);
            }
        }
    }
}
