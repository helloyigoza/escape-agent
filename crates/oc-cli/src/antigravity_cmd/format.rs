//! Human-readable account and model formatting shared by the auth commands and
//! the `/ag` chat commands.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use oc_antigravity::AccountStatusSummary;

/// Format all accounts cleanly for CLI stdout/stderr display.
pub(crate) fn format_accounts_text(accounts: &[AccountStatusSummary]) -> String {
    if accounts.is_empty() {
        return "  No Google Antigravity accounts configured. Run `opencode auth login`.\n"
            .to_owned();
    }
    let now = current_epoch_ms();
    let mut out = String::new();
    for acct in accounts {
        let mut tags = Vec::new();
        if acct.active_claude {
            tags.push("claude".to_owned());
        }
        if acct.active_gemini {
            tags.push("gemini".to_owned());
        }
        if !acct.enabled {
            tags.push("disabled".to_owned());
        }
        if acct.revoked {
            tags.push("revoked".to_owned());
        }
        if acct.verification_required {
            tags.push("verification_required".to_owned());
        }
        let active_mark = if acct.active_claude || acct.active_gemini {
            "*"
        } else {
            " "
        };
        let status_tags = if tags.is_empty() {
            "ready".to_owned()
        } else {
            tags.join(",")
        };
        let proj = acct.project_id.as_deref().unwrap_or("-");
        let tier = acct.tier.as_deref().unwrap_or("-");
        let _ = writeln!(
            out,
            "  {active_mark} [{}] {}  (id={}, proj={}, tier={}, status={})",
            acct.index,
            format_identity(acct),
            acct.id,
            proj,
            tier,
            status_tags
        );
        if let Some(url) = &acct.verification_url
            && acct.verification_required
        {
            let _ = writeln!(out, "      verify at: {url}");
        }
        let mut active_limits: Vec<(&str, i64)> = acct
            .rate_limits
            .iter()
            .filter_map(|(k, reset_at)| (*reset_at > now).then_some((k.as_str(), *reset_at - now)))
            .collect();
        active_limits.sort_by_key(|(k, _)| *k);
        if !active_limits.is_empty() {
            let formatted: Vec<String> = active_limits
                .into_iter()
                .map(|(k, rem)| format!("{k} ({})", format_duration_ms(rem)))
                .collect();
            let _ = writeln!(out, "      cooldowns: {}", formatted.join(", "));
        }
    }
    out
}

pub(super) fn format_models_text(models: &[oc_llm::ModelInfo]) -> String {
    let mut out = String::new();
    for m in models {
        let variants = if m.variants.is_empty() {
            String::new()
        } else {
            let names: Vec<&str> = m.variants.iter().map(|v| v.id.as_str()).collect();
            format!(" [variants: {}]", names.join(", "))
        };
        let _ = writeln!(
            out,
            "  google/{:<42} {} (ctx={}k, out={}k){}",
            m.id,
            m.name,
            m.limit.context / 1000,
            m.limit.output / 1000,
            variants
        );
    }
    out
}

pub(super) fn format_identity(acct: &AccountStatusSummary) -> String {
    match (&acct.email, &acct.label) {
        (Some(email), Some(label)) if !label.is_empty() => format!("{email} [{label}]"),
        (Some(email), _) => email.clone(),
        (None, Some(label)) if !label.is_empty() => format!("{label} ({})", acct.id),
        (None, _) => acct.id.clone(),
    }
}

pub(super) fn format_duration_ms(delta_ms: i64) -> String {
    let secs = (delta_ms.max(0)) / 1000;
    let mins = secs / 60;
    let hours = mins / 60;
    if hours > 0 {
        format!("{hours}h {}m", mins % 60)
    } else if mins > 0 {
        format!("{mins}m {}s", secs % 60)
    } else {
        format!("{secs}s")
    }
}

pub(super) fn current_epoch_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}
