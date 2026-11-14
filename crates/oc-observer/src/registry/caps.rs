//! Retention caps, the snapshot shape and the small text helpers they need.

use super::strip::FlightStrip;

/// Maximum characters retained from an `outcome` one-liner.
pub const MAX_OUTCOME_CHARS: usize = 60;
/// Maximum characters retained from `why`.
pub const MAX_WHY_CHARS: usize = 160;
/// Settled strips are pruned after 24 hours.
pub const SETTLED_TTL_MS: i64 = 24 * 60 * 60 * 1_000;
/// Settled strips retained per root before the oldest are dropped.
pub const MAX_SETTLED_PER_ROOT: usize = 10_000;
/// Default active window for [`super::ExpectationRegistry::snapshot`].
pub const SNAPSHOT_ACTIVE: usize = 24;
/// Milliseconds in one second, for the `expect_s`/`intervene_s` conversions.
pub const MS_PER_SECOND: i64 = 1_000;
/// Retention knobs for the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryCaps {
    /// Settled strips retained per root.
    pub max_settled_per_root: usize,
    /// Settled strip time-to-live.
    pub settled_ttl_ms: i64,
}

impl Default for RegistryCaps {
    fn default() -> Self {
        Self {
            max_settled_per_root: MAX_SETTLED_PER_ROOT,
            settled_ttl_ms: SETTLED_TTL_MS,
        }
    }
}

/// A point-in-time view of one root's strips.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistrySnapshot {
    /// Running strips, most urgent first (overdue, then earliest deadline).
    pub active: Vec<FlightStrip>,
    /// Settled strips, newest first.
    pub recent: Vec<FlightStrip>,
}
pub(super) fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        text.chars().take(max).collect()
    }
}
