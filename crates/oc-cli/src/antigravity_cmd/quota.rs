//! Quota rendering: live refresh reports, cached per-group snapshots and
//! `opencode auth quota`.

use std::fmt::Write as _;

use oc_antigravity::{
    AccountStatusSummary, AntigravityService, QuotaGroupSnapshot, QuotaRefreshReport,
};

use super::format::{current_epoch_ms, format_duration_ms, format_identity};

pub(super) async fn cmd_quota(
    svc: &AntigravityService,
    cached: bool,
    json_mode: bool,
) -> anyhow::Result<()> {
    if cached {
        let accounts = svc.list_accounts_status()?;
        if json_mode {
            println!("{}", serde_json::to_string_pretty(&accounts)?);
        } else {
            print!("{}", format_cached_quotas_text(&accounts));
        }
        return Ok(());
    }

    match svc.refresh_quotas_and_models().await {
        Ok(report) => {
            if json_mode {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", format_quota_report(&report));
            }
        }
        Err(err) => {
            let accounts = svc.list_accounts_status()?;
            if accounts.is_empty() {
                return Err(err.into());
            }
            eprintln!("Warning: live quota fetch failed ({err}); showing cached data.");
            if json_mode {
                println!("{}", serde_json::to_string_pretty(&accounts)?);
            } else {
                print!("{}", format_cached_quotas_text(&accounts));
            }
        }
    }
    Ok(())
}

/// Format a live quota refresh report.
pub(crate) fn format_quota_report(report: &QuotaRefreshReport) -> String {
    let mut out = format_cached_quotas_text(&report.accounts);
    let _ = writeln!(
        out,
        "  Discovered upstream models: {}",
        report.discovered_model_count
    );
    for warn in &report.warnings {
        let _ = writeln!(out, "  warning: {warn}");
    }
    out
}

/// Format cached per-group quotas for all accounts.
pub(crate) fn format_cached_quotas_text(accounts: &[AccountStatusSummary]) -> String {
    if accounts.is_empty() {
        return "  No Google Antigravity accounts configured.\n".to_owned();
    }
    let now = current_epoch_ms();
    let mut out = String::new();
    for acct in accounts {
        let _ = writeln!(
            out,
            "  [{}] {} (tier={})",
            acct.index,
            format_identity(acct),
            acct.tier.as_deref().unwrap_or("-")
        );
        let Some(quota) = &acct.quota else {
            let _ = writeln!(out, "      (no quota snapshot yet)");
            continue;
        };
        if quota.groups.is_empty() {
            let _ = writeln!(out, "      (no quota groups reported)");
            continue;
        }
        for (group, snap) in &quota.groups {
            let pct =
                remaining_percent(snap).map_or_else(|| "?".to_owned(), |p| format!("{p:.0}% left"));
            let reset = snap
                .reset_time_ms
                .filter(|ms| *ms > now)
                .map_or_else(String::new, |ms| {
                    format!(" (resets in {})", format_duration_ms(ms - now))
                });
            let _ = writeln!(out, "      {group:<14} {pct}{reset}");
        }
    }
    out
}

pub(super) fn remaining_percent(snap: &QuotaGroupSnapshot) -> Option<f64> {
    if let Some(frac) = snap.remaining_fraction {
        let pct = if frac <= 1.0 { frac * 100.0 } else { frac };
        return Some(pct.clamp(0.0, 100.0));
    }
    snap.used_percent
        .map(|used| (100.0 - used).clamp(0.0, 100.0))
}
