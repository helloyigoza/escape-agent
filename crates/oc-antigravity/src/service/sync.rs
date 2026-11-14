//! Account-pool <-> SQLite credential synchronization and shared account-lock helpers.

use super::types::{AccountStatusSummary, CachedAccessToken};
use super::{AntigravityService, now_ms};
use crate::accounts::{AccountManager, ModelFamily};
use crate::auth::parse_refresh_parts;
use crate::constants::{ANTIGRAVITY_METHOD_ID, ANTIGRAVITY_PROVIDER_ID};
use crate::error::{AntigravityError, Result};
use crate::fingerprint::generate_fingerprint;
use crate::oauth::AntigravityTokenExchange;
use crate::storage::{AccountMetadataV4, account_token_hash};
use oc_store::{CredentialPatch, NewCredential};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

impl AntigravityService {
    /// Import any `"google"` `"oauth"` credentials from `oc_store::Store` into the account pool.
    pub async fn sync_from_store(&self) -> Result<()> {
        let Some(ref store) = self.inner.store else {
            return Ok(());
        };
        let rows = store.list_credentials().await?;
        let now = now_ms();

        let mut mgr = lock_accounts(&self.inner.accounts)?;
        for row in rows {
            if row.get("integrationID").and_then(Value::as_str) != Some(ANTIGRAVITY_PROVIDER_ID) {
                continue;
            }
            let Some(val) = row.get("value") else {
                continue;
            };
            if val.get("type").and_then(Value::as_str) != Some("oauth") {
                continue;
            }
            let Some(refresh_raw) = val.get("refresh").and_then(Value::as_str) else {
                continue;
            };
            if refresh_raw.trim().is_empty() {
                continue;
            }
            let parts = parse_refresh_parts(refresh_raw);
            let cred_id = row.get("id").and_then(Value::as_str).map(str::to_owned);
            let label = row.get("label").and_then(Value::as_str).map(str::to_owned);
            let email = val
                .pointer("/metadata/email")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let active = row.get("active").and_then(Value::as_bool).unwrap_or(false);

            // Cache non-empty unexpired access token in memory right away.
            if let (Some(access), Some(expires)) = (
                val.get("access").and_then(Value::as_str),
                val.get("expires").and_then(Value::as_i64),
            ) && !access.trim().is_empty()
            {
                let hash = account_token_hash(&parts.refresh_token);
                if let Ok(mut tc) = self.inner.token_cache.lock() {
                    tc.insert(
                        hash,
                        CachedAccessToken {
                            access: access.to_owned(),
                            expires,
                        },
                    );
                }
            }

            let exists = mgr.accounts().iter().any(|a| {
                a.refresh_token == parts.refresh_token || a.store_credential_id == cred_id
            });
            if !exists {
                let acct = AccountMetadataV4 {
                    email,
                    label,
                    refresh_token: parts.refresh_token,
                    project_id: parts.project_id,
                    managed_project_id: parts.managed_project_id,
                    added_at: now,
                    last_used: 0,
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
                    token_hash: String::new(),
                    store_credential_id: cred_id,
                };
                let _ = mgr.upsert_account(acct, active);
            }
        }
        Ok(())
    }

    pub(super) async fn sync_credential_to_store(
        &self,
        exchange: &AntigravityTokenExchange,
        label: Option<String>,
    ) -> Result<Option<String>> {
        let Some(ref store) = self.inner.store else {
            return Ok(None);
        };

        let existing = store.list_credentials().await?;
        let matching_id = existing.iter().find_map(|row| {
            if row.get("integrationID").and_then(Value::as_str) != Some(ANTIGRAVITY_PROVIDER_ID) {
                return None;
            }
            let val = row.get("value")?;
            if val.get("type").and_then(Value::as_str) != Some("oauth") {
                return None;
            }
            let same_email = exchange.email.as_deref().is_some_and(|em| {
                val.pointer("/metadata/email")
                    .and_then(Value::as_str)
                    .is_some_and(|existing_em| existing_em.eq_ignore_ascii_case(em))
            });
            let same_refresh = val
                .get("refresh")
                .and_then(Value::as_str)
                .is_some_and(|r| parse_refresh_parts(r).refresh_token == exchange.refresh_token);
            if same_email || same_refresh {
                row.get("id").and_then(Value::as_str).map(str::to_owned)
            } else {
                None
            }
        });

        let mut meta = serde_json::Map::new();
        if let Some(ref email) = exchange.email {
            meta.insert("email".to_owned(), json!(email));
        }
        meta.insert("projectId".to_owned(), json!(exchange.project_id));
        if let Some(ref mpid) = exchange.managed_project_id {
            meta.insert("managedProjectId".to_owned(), json!(mpid));
        }

        let value = json!({
            "type": "oauth",
            "methodID": ANTIGRAVITY_METHOD_ID,
            "refresh": exchange.refresh,
            "access": exchange.access,
            "expires": exchange.expires,
            "metadata": Value::Object(meta),
        });

        if let Some(cid) = matching_id {
            let _ = store
                .update_credential(
                    &cid,
                    CredentialPatch {
                        label: label.or_else(|| exchange.email.clone()),
                        value: Some(value),
                        active: Some(true),
                    },
                )
                .await?;
            Ok(Some(cid))
        } else {
            let created = store
                .create_credential(NewCredential {
                    id: None,
                    integration_id: ANTIGRAVITY_PROVIDER_ID.to_owned(),
                    label: label.or_else(|| exchange.email.clone()),
                    value,
                    active: Some(true),
                })
                .await?;
            Ok(created.get("id").and_then(Value::as_str).map(str::to_owned))
        }
    }
}

pub(super) fn lock_accounts(
    mutex: &StdMutex<AccountManager>,
) -> Result<std::sync::MutexGuard<'_, AccountManager>> {
    mutex
        .lock()
        .map_err(|_| AntigravityError::Storage("account manager lock poisoned".to_owned()))
}

pub(super) fn build_account_summary(
    mgr: &AccountManager,
    index: usize,
    now: i64,
) -> Option<AccountStatusSummary> {
    let acct = mgr.accounts().get(index)?;
    let active_claude = mgr.active_cursor(ModelFamily::Claude) == index;
    let active_gemini = mgr.active_cursor(ModelFamily::Gemini) == index;
    Some(AccountStatusSummary {
        index,
        id: acct.token_hash.clone(),
        email: acct.email.clone(),
        label: acct.label.clone(),
        active_claude,
        active_gemini,
        enabled: acct.enabled,
        verification_required: acct.verification_required,
        verification_url: acct.verification_url.clone(),
        revoked: acct.revoked,
        project_id: acct
            .project_id
            .clone()
            .or_else(|| acct.managed_project_id.clone()),
        tier: acct.cached_tier.clone(),
        rate_limits: mgr.active_rate_limits(index, now),
        quota: acct.cached_quota.clone(),
        added_at: acct.added_at,
        last_used: acct.last_used,
    })
}
