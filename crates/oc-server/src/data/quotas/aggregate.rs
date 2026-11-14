//! The `GET /api/quotas` assembly: provider set, probe pass, ordering and the
//! spend summary.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use oc_llm::quota::now_epoch_ms;
use oc_llm::{ProviderRateLimit, QuotaCapability};
use oc_types::now_millis;

use crate::AppState;
use crate::data::model::active_provider_ids;
use crate::error::ErrorResponse;

use super::antigravity::refresh_antigravity_if_needed;
use super::openrouter::resolve_openrouter_management_key;
use super::providers::{ProviderEntryInput, build_provider_entry, probe_live_providers};
use super::types::{ProviderQuotaEntry, UnifiedQuotaSummary, UnifiedQuotasResponse};
use super::usage::usage_for;

pub(super) async fn build_unified_quotas(
    state: &AppState,
    force_refresh: bool,
) -> Result<UnifiedQuotasResponse, ErrorResponse> {
    let now_ms = now_millis().max(now_epoch_ms());
    let credentials = match state.store() {
        Some(store) => store.list_credentials().await?,
        None => Vec::new(),
    };
    let catalog = state.catalog().map(Arc::as_ref);
    let config = state.config().map(Arc::as_ref);

    // Only providers with a genuinely active credential (or a usable
    // Antigravity account) belong in the report. Providers that merely have
    // historical local usage or a captured rate-limit snapshot do **not**: they
    // would render a card the user cannot act on. `available_provider_ids` is
    // intentionally not used here — it keeps inactive providers visible to the
    // model/provider routes.
    let active_ids = match catalog {
        Some(cat) => active_provider_ids(state, cat).await,
        None => BTreeSet::new(),
    };

    let usage_map = usage_for(state, now_ms, force_refresh).await?;
    let mut rate_limit_map = collect_rate_limits(state);

    // Active providers only; local usage is attached to those that appear.
    let provider_ids: BTreeSet<String> = active_ids.clone();

    let ag_refresh = refresh_antigravity_if_needed(state, force_refresh).await;
    let probe_results = probe_live_providers(
        state,
        &active_ids,
        catalog,
        config,
        &credentials,
        force_refresh,
        now_ms,
    )
    .await;

    let openrouter_key = resolve_openrouter_management_key(&credentials, config);
    let openrouter_mgmt = openrouter_key.is_some();
    let openrouter_source = openrouter_key.as_ref().map(|key| key.source);
    let mut providers = Vec::with_capacity(provider_ids.len());
    for id in provider_ids {
        // Every id in this set came from `active_provider_ids`, so the entry is
        // connected by construction; nothing re-adds an inactive provider.
        let connected = active_ids.contains(&id);
        let usage = usage_map.get(&id).cloned();
        let rate_limit = rate_limit_map.remove(&id);
        let probe = probe_results.get(&id);
        let entry = build_provider_entry(ProviderEntryInput {
            provider_id: id,
            connected,
            catalog,
            config,
            credentials: &credentials,
            ag_refresh: ag_refresh.as_ref(),
            probe,
            rate_limit,
            usage,
            openrouter_mgmt,
            openrouter_source,
        });
        providers.push(entry);
    }

    providers.sort_by(|a, b| {
        provider_sort_rank(a)
            .cmp(&provider_sort_rank(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.provider_id.cmp(&b.provider_id))
    });

    let summary = summarize_providers(&providers);
    Ok(UnifiedQuotasResponse {
        fetched_at_ms: now_ms,
        summary,
        providers,
    })
}

fn provider_sort_rank(entry: &ProviderQuotaEntry) -> u8 {
    if !entry.connected {
        return 3;
    }
    match entry.capability {
        QuotaCapability::LiveApi if !entry.windows.is_empty() || !entry.balances.is_empty() => 0,
        QuotaCapability::LiveApi => 1,
        QuotaCapability::RateLimitHeadersOnly => 2,
    }
}

fn summarize_providers(providers: &[ProviderQuotaEntry]) -> UnifiedQuotaSummary {
    let mut live_quota_count = 0_usize;
    let mut five_hours_usd = 0.0_f64;
    let mut day_usd = 0.0_f64;
    let mut week_usd = 0.0_f64;
    let mut month_usd = 0.0_f64;

    for entry in providers {
        if entry.capability == QuotaCapability::LiveApi
            && (!entry.windows.is_empty() || !entry.balances.is_empty())
        {
            live_quota_count += 1;
        }
        if let Some(usage) = &entry.local_usage {
            five_hours_usd += usage.five_hours.cost_usd;
            day_usd += usage.day.cost_usd;
            week_usd += usage.week.cost_usd;
            month_usd += usage.month.cost_usd;
        }
    }

    UnifiedQuotaSummary {
        provider_count: providers.len(),
        live_quota_count,
        spend5h_usd: five_hours_usd,
        spend24h_usd: day_usd,
        spend7d_usd: week_usd,
        spend30d_usd: month_usd,
    }
}

fn collect_rate_limits(state: &AppState) -> BTreeMap<String, ProviderRateLimit> {
    let mut map = BTreeMap::new();
    if let Some(llm) = state.llm() {
        for item in llm.rate_limits() {
            map.insert(item.provider_id.clone(), item);
        }
    }
    map
}
