//! `opencode auth list|status|use|enable|disable|remove|models`: the
//! multi-account pool surface.

use anyhow::bail;
use oc_antigravity::{AccountStatusSummary, AntigravityService};
use serde_json::json;

use super::auth::{is_antigravity_provider, normalize_provider_name};
use super::format::{format_accounts_text, format_identity, format_models_text};

pub(super) async fn cmd_list(
    store: &oc_store::Store,
    svc: &AntigravityService,
    provider_filter: Option<&str>,
    json_mode: bool,
) -> anyhow::Result<()> {
    let mut credentials = store.list_credentials().await?;
    if let Some(filter) = provider_filter.map(normalize_provider_name) {
        credentials.retain(|c| {
            c.get("integrationID")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|p| p.eq_ignore_ascii_case(&filter))
        });
    }
    let show_ag = provider_filter.is_none_or(is_antigravity_provider);
    let accounts = if show_ag {
        svc.list_accounts_status()?
    } else {
        Vec::new()
    };

    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "credentials": credentials,
                "antigravityAccounts": accounts,
            }))?
        );
        return Ok(());
    }

    if credentials.is_empty() {
        println!("No stored credentials in database.");
    } else {
        println!("Stored credentials:");
        for cred in &credentials {
            let id = cred.get("id").and_then(|v| v.as_str()).unwrap_or("-");
            let prov = cred
                .get("integrationID")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let label = cred
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("default");
            let active = cred
                .get("active")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let marker = if active { "*" } else { " " };
            println!("  {marker} {id}  provider={prov}  label={label}");
        }
    }

    if show_ag {
        println!("\nGoogle Antigravity pool:");
        print!("{}", format_accounts_text(&accounts));
    }
    Ok(())
}

pub(super) async fn cmd_status(
    svc: &AntigravityService,
    refresh: bool,
    json_mode: bool,
) -> anyhow::Result<()> {
    let accounts = if refresh {
        svc.refresh_quotas_and_models().await?.accounts
    } else {
        svc.list_accounts_status()?
    };
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "count": accounts.len(),
                "usableCount": svc.usable_account_count(),
                "accounts": accounts,
            }))?
        );
    } else {
        print!("{}", format_accounts_text(&accounts));
    }
    Ok(())
}

pub(super) async fn cmd_remove(
    store: &oc_store::Store,
    svc: &AntigravityService,
    account: &str,
    json_mode: bool,
) -> anyhow::Result<()> {
    if account.starts_with("cred_") {
        let removed_ag = svc.remove_by_credential_id(account);
        let removed_db = store.delete_credential(account).await?;
        if !removed_ag && !removed_db {
            bail!("credential `{account}` not found");
        }
    } else {
        svc.remove_account(account).await?;
    }

    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "removed": account,
                "remainingCount": svc.account_count(),
            }))?
        );
    } else {
        println!(
            "Removed `{account}`. {} account(s) remaining.",
            svc.account_count()
        );
    }
    Ok(())
}

pub(super) async fn cmd_models(
    svc: &AntigravityService,
    refresh: bool,
    json_mode: bool,
) -> anyhow::Result<()> {
    let mut discovered_count = svc.cached_discovered_models().models.len();
    if refresh {
        let report = svc.refresh_quotas_and_models().await?;
        discovered_count = report.discovered_model_count;
    }
    let models = svc.antigravity_model_infos();
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "discoveredCount": discovered_count,
                "count": models.len(),
                "models": models,
            }))?
        );
    } else {
        println!(
            "Google Antigravity models ({} total, {} discovered):",
            models.len(),
            discovered_count
        );
        print!("{}", format_models_text(&models));
    }
    Ok(())
}

pub(super) fn print_single_account(
    heading: &str,
    summary: &AccountStatusSummary,
    json_mode: bool,
) -> anyhow::Result<()> {
    if json_mode {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        println!("{heading} {}", format_identity(summary));
    }
    Ok(())
}
