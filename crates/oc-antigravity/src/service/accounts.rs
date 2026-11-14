//! Account pool inspection and mutation: status listing, selection, enable/disable, removal.

use super::sync::{build_account_summary, lock_accounts};
use super::types::AccountStatusSummary;
use super::{AntigravityService, now_ms};
use crate::error::{AntigravityError, Result};

impl AntigravityService {
    /// List all accounts in the pool with health, rate limits, and cached quota.
    pub fn list_accounts_status(&self) -> Result<Vec<AccountStatusSummary>> {
        let now = now_ms();
        let mgr = lock_accounts(&self.inner.accounts)?;
        Ok((0..mgr.len())
            .filter_map(|i| build_account_summary(&mgr, i, now))
            .collect())
    }

    /// Select the active sticky account by index, email, label, or hash.
    pub async fn select_account(&self, identifier: &str) -> Result<AccountStatusSummary> {
        let now = now_ms();
        let (summary, cred_id) = {
            let mut mgr = lock_accounts(&self.inner.accounts)?;
            let idx = mgr.find_account(identifier).ok_or_else(|| {
                AntigravityError::NoAccounts(format!("account `{identifier}` not found"))
            })?;
            mgr.select_account(idx)?;
            let cred_id = mgr.accounts()[idx].store_credential_id.clone();
            let sum = build_account_summary(&mgr, idx, now).ok_or_else(|| {
                AntigravityError::NoAccounts("selected account missing".to_owned())
            })?;
            (sum, cred_id)
        };

        if let (Some(store), Some(cid)) = (&self.inner.store, cred_id) {
            let _ = store.activate_credential(&cid).await;
        }
        Ok(summary)
    }

    /// Enable or disable an account by index, email, label, or hash.
    pub fn set_account_enabled(
        &self,
        identifier: &str,
        enabled: bool,
    ) -> Result<AccountStatusSummary> {
        let now = now_ms();
        let mut mgr = lock_accounts(&self.inner.accounts)?;
        let idx = mgr.find_account(identifier).ok_or_else(|| {
            AntigravityError::NoAccounts(format!("account `{identifier}` not found"))
        })?;
        mgr.set_enabled(idx, enabled)?;
        if enabled {
            mgr.clear_verification_required(idx)?;
        }
        build_account_summary(&mgr, idx, now)
            .ok_or_else(|| AntigravityError::NoAccounts("account missing".to_owned()))
    }

    /// Remove an account from the pool (and delete linked `oc-store` credential if present).
    pub async fn remove_account(&self, identifier: &str) -> Result<()> {
        let removed = {
            let mut mgr = lock_accounts(&self.inner.accounts)?;
            let idx = mgr.find_account(identifier).ok_or_else(|| {
                AntigravityError::NoAccounts(format!("account `{identifier}` not found"))
            })?;
            mgr.remove_account(idx)?
        };

        if let Ok(mut tc) = self.inner.token_cache.lock() {
            tc.remove(&removed.token_hash);
        }
        if let (Some(store), Some(cid)) = (&self.inner.store, removed.store_credential_id) {
            let _ = store.delete_credential(&cid).await;
        }
        Ok(())
    }

    /// Remove any account linked to `store_credential_id`, returning true if removed.
    pub fn remove_by_credential_id(&self, cred_id: &str) -> bool {
        let Ok(mut mgr) = lock_accounts(&self.inner.accounts) else {
            return false;
        };
        let Some(idx) = mgr
            .accounts()
            .iter()
            .position(|a| a.store_credential_id.as_deref() == Some(cred_id))
        else {
            return false;
        };
        if let Ok(removed) = mgr.remove_account(idx) {
            if let Ok(mut tc) = self.inner.token_cache.lock() {
                tc.remove(&removed.token_hash);
            }
            return true;
        }
        false
    }

    /// Select any account linked to `store_credential_id`, returning true if found.
    pub fn select_by_credential_id(&self, cred_id: &str) -> bool {
        let Ok(mut mgr) = lock_accounts(&self.inner.accounts) else {
            return false;
        };
        let Some(idx) = mgr
            .accounts()
            .iter()
            .position(|a| a.store_credential_id.as_deref() == Some(cred_id))
        else {
            return false;
        };
        mgr.select_account(idx).is_ok()
    }
}
