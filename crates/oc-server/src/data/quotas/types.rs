//! Wire DTOs for the unified quota response and the local usage rollup.
//!
//! Every shape here is read by the settings UI, so the serde attributes are
//! part of the contract; the facade owns the handlers that serialize them.

use std::collections::BTreeMap;

use oc_antigravity::AccountStatusSummary;
use oc_llm::{
    ProviderApiSpend, ProviderBalanceSummary, ProviderRateLimit, QuotaCapability, QuotaWindow,
};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UnifiedQuotasResponse {
    pub(super) fetched_at_ms: i64,
    pub(super) summary: UnifiedQuotaSummary,
    pub(super) providers: Vec<ProviderQuotaEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UnifiedQuotaSummary {
    pub(super) provider_count: usize,
    pub(super) live_quota_count: usize,
    pub(super) spend5h_usd: f64,
    pub(super) spend24h_usd: f64,
    pub(super) spend7d_usd: f64,
    pub(super) spend30d_usd: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProviderQuotaEntry {
    #[serde(rename = "providerID")]
    pub(super) provider_id: String,
    pub(super) name: String,
    pub(super) connected: bool,
    pub(super) connection_source: String,
    pub(super) capability: QuotaCapability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) capability_note: Option<String>,
    pub(super) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) fetched_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) cached: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) has_management_key: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) management_key_source: Option<ManagementKeySource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) key_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) rate_limit_rule: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) windows: Vec<QuotaWindowEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) balances: Vec<ProviderBalanceSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) spend_summary: Option<ProviderApiSpend>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) rate_limit: Option<ProviderRateLimit>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) accounts: Vec<AccountStatusSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) local_usage: Option<ProviderLocalUsage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

/// Where the `OpenRouter` management key the probe uses comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum ManagementKeySource {
    /// `OPENROUTER_MANAGEMENT_KEY` owns the key; stored state cannot win.
    Env,
    /// Stored as a credential (metadata or the synthetic management credential).
    Credential,
    /// Supplied by `providers.openrouter.settings.managementKey`.
    Config,
}

/// A [`QuotaWindow`] plus the stable `groupKey` a client localizes on.
///
/// `label` stays on the wire as the server-side English fallback.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct QuotaWindowEntry {
    pub(super) id: String,
    pub(super) label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) group_key: Option<String>,
    pub(super) unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) limit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) used: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) remaining: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) used_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) remaining_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) limited: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reset_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reset_label: Option<String>,
}

impl QuotaWindowEntry {
    /// Project a probe window; provider APIs have no pool grouping.
    pub(super) fn from_probe(window: &QuotaWindow) -> Self {
        Self {
            id: window.id.clone(),
            label: window.label.clone(),
            group_key: None,
            unit: window.unit.clone(),
            limit: window.limit,
            used: window.used,
            remaining: window.remaining,
            used_percent: window.used_percent,
            remaining_percent: window.remaining_percent,
            limited: window.limited,
            reset_at_ms: window.reset_at_ms,
            reset_label: window.reset_label.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UsageWindowTotals {
    pub(super) steps: i64,
    pub(super) input_tokens: i64,
    pub(super) output_tokens: i64,
    pub(super) reasoning_tokens: i64,
    pub(super) cache_read_tokens: i64,
    pub(super) cache_write_tokens: i64,
    pub(super) total_tokens: i64,
    pub(super) cost_usd: f64,
}

impl UsageWindowTotals {
    pub(super) fn add(&mut self, msg: &Value) {
        let input = msg
            .pointer("/tokens/input")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let output = msg
            .pointer("/tokens/output")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let reasoning = msg
            .pointer("/tokens/reasoning")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let cache_read = msg
            .pointer("/tokens/cache/read")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let cache_write = msg
            .pointer("/tokens/cache/write")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let cost = msg.get("cost").and_then(Value::as_f64).unwrap_or(0.0);

        self.steps += 1;
        self.input_tokens += input;
        self.output_tokens += output;
        self.reasoning_tokens += reasoning;
        self.cache_read_tokens += cache_read;
        self.cache_write_tokens += cache_write;
        self.total_tokens += input + output + reasoning + cache_read + cache_write;
        if cost.is_finite() {
            self.cost_usd += cost;
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ModelUsageRow {
    #[serde(rename = "modelID")]
    pub(super) model_id: String,
    pub(super) steps: i64,
    pub(super) total_tokens: i64,
    pub(super) cost_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProviderLocalUsage {
    pub(super) five_hours: UsageWindowTotals,
    pub(super) day: UsageWindowTotals,
    pub(super) week: UsageWindowTotals,
    pub(super) month: UsageWindowTotals,
    pub(super) all_time: UsageWindowTotals,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) top_models: Vec<ModelUsageRow>,
}

#[derive(Default)]
pub(super) struct ProviderUsageAccum {
    pub(super) five_hours: UsageWindowTotals,
    pub(super) day: UsageWindowTotals,
    pub(super) week: UsageWindowTotals,
    pub(super) month: UsageWindowTotals,
    pub(super) all_time: UsageWindowTotals,
    pub(super) models: BTreeMap<String, (i64, i64, f64)>,
}
