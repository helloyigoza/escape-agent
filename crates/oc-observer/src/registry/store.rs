//! Registry internals: per-root indexes, settlement, pruning and snapshots.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::clock::{Clock, EngineNow};

use super::ExpectationRegistry;
use super::caps::{
    MAX_OUTCOME_CHARS, MAX_WHY_CHARS, RegistryCaps, RegistrySnapshot, SNAPSHOT_ACTIVE,
    truncate_chars,
};
use super::strip::{FlightStrip, StripState};

#[derive(Debug, Default)]
struct RootIndex {
    /// Running tool-call ids in open order.
    active: Vec<String>,
    /// Settled tool-call ids, newest first.
    settled: VecDeque<String>,
}

impl RootIndex {
    fn remove_active(&mut self, tool_call_id: &str) -> bool {
        if let Some(pos) = self.active.iter().position(|id| id == tool_call_id) {
            self.active.remove(pos);
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct RegistryData {
    strips: HashMap<String, FlightStrip>,
    roots: HashMap<String, RootIndex>,
}

impl ExpectationRegistry {
    /// Create a registry with default retention caps.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self::with_caps(clock, RegistryCaps::default())
    }

    /// Create a registry with explicit retention caps.
    #[must_use]
    pub fn with_caps(clock: Arc<dyn Clock>, caps: RegistryCaps) -> Self {
        Self {
            clock,
            caps,
            data: Mutex::new(RegistryData::default()),
        }
    }

    /// Retention caps.
    #[must_use]
    pub fn caps(&self) -> RegistryCaps {
        self.caps
    }

    /// Register a new strip. Running strips join the root's active index;
    /// already-settled strips join the recent index.
    pub fn open(&self, mut strip: FlightStrip) {
        strip.why = truncate_chars(&strip.why, MAX_WHY_CHARS);
        strip.outcome = strip
            .outcome
            .as_deref()
            .map(|outcome| truncate_chars(outcome, MAX_OUTCOME_CHARS));
        if strip.id.is_empty() {
            strip.id = format!("obs_{}", strip.tool_call_id);
        }

        let tool_call_id = strip.tool_call_id.clone();
        let root_id = strip.root_id.clone();
        let state = strip.state;
        let now = EngineNow::from_clock(self.clock.as_ref());
        let mut data = self.data.lock();

        if let Some(previous) = data.strips.remove(&tool_call_id)
            && let Some(index) = data.roots.get_mut(&previous.root_id)
        {
            index.remove_active(&tool_call_id);
            index.settled.retain(|id| id != &tool_call_id);
        }

        data.strips.insert(tool_call_id.clone(), strip);
        let index = data.roots.entry(root_id.clone()).or_default();
        if state == StripState::Running {
            index.active.push(tool_call_id);
        } else {
            index.settled.push_front(tool_call_id);
        }
        prune_root(&mut data, &root_id, self.caps, now.mono_ms);
    }

    /// Settle a strip. Returns false when the tool call is unknown.
    pub fn settle(&self, tool_call_id: &str, state: StripState, outcome: Option<&str>) -> bool {
        let now = EngineNow::from_clock(self.clock.as_ref());
        self.settle_at(tool_call_id, state, outcome, now)
    }

    /// Settle a strip at an explicit pair of clock readings.
    pub fn settle_at(
        &self,
        tool_call_id: &str,
        state: StripState,
        outcome: Option<&str>,
        now: EngineNow,
    ) -> bool {
        let mut data = self.data.lock();
        let Some(strip) = data.strips.get_mut(tool_call_id) else {
            return false;
        };
        let root_id = strip.root_id.clone();
        let was_running = strip.state == StripState::Running;
        strip.state = state;
        strip.settled_ms = Some(now.wall_ms);
        strip.settled_mono_ms = Some(now.mono_ms);
        strip.outcome = outcome.map(|text| truncate_chars(text, MAX_OUTCOME_CHARS));

        if let Some(index) = data.roots.get_mut(&root_id) {
            index.remove_active(tool_call_id);
            if was_running {
                index.settled.push_front(tool_call_id.to_owned());
            }
        }
        prune_root(&mut data, &root_id, self.caps, now.mono_ms);
        true
    }

    /// Cancel every running strip of a session (root cancelled or interrupted).
    /// Returns the number of strips settled.
    pub fn cancel_session(&self, session_id: &str) -> usize {
        let now = EngineNow::from_clock(self.clock.as_ref());
        let mut data = self.data.lock();
        let ids: Vec<String> = data
            .strips
            .values()
            .filter(|strip| strip.session_id == session_id && strip.state == StripState::Running)
            .map(|strip| strip.tool_call_id.clone())
            .collect();
        let mut roots = HashSet::new();
        for id in &ids {
            if let Some(strip) = data.strips.get_mut(id) {
                strip.state = StripState::Cancelled;
                strip.settled_ms = Some(now.wall_ms);
                strip.settled_mono_ms = Some(now.mono_ms);
                strip.outcome = Some("cancelled".to_owned());
                roots.insert(strip.root_id.clone());
            }
            if let Some(root_id) = data.strips.get(id).map(|strip| strip.root_id.clone())
                && let Some(index) = data.roots.get_mut(&root_id)
            {
                index.remove_active(id);
                index.settled.push_front(id.clone());
            }
        }
        for root_id in roots {
            prune_root(&mut data, &root_id, self.caps, now.mono_ms);
        }
        ids.len()
    }

    /// Running strips for a root, in open order.
    #[must_use]
    pub fn active(&self, root_id: &str) -> Vec<FlightStrip> {
        let data = self.data.lock();
        let Some(index) = data.roots.get(root_id) else {
            return Vec::new();
        };
        index
            .active
            .iter()
            .filter_map(|id| data.strips.get(id))
            .filter(|strip| strip.state == StripState::Running)
            .cloned()
            .collect()
    }

    /// Settled strips for a root, newest first.
    #[must_use]
    pub fn recent(&self, root_id: &str, limit: usize) -> Vec<FlightStrip> {
        let data = self.data.lock();
        let Some(index) = data.roots.get(root_id) else {
            return Vec::new();
        };
        index
            .settled
            .iter()
            .filter_map(|id| data.strips.get(id))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Earliest intervention deadline among running strips, as an absolute
    /// wall-clock instant.
    ///
    /// The remaining time comes from the monotonic clock and is added to the
    /// current wall instant, so a wall-clock jump cannot move the result.
    #[must_use]
    pub fn earliest_deadline(&self, root_id: &str) -> Option<i64> {
        let now = EngineNow::from_clock(self.clock.as_ref());
        self.earliest_deadline_at(root_id, now)
    }

    /// [`Self::earliest_deadline`] at an explicit pair of clock readings.
    #[must_use]
    pub fn earliest_deadline_at(&self, root_id: &str, now: EngineNow) -> Option<i64> {
        self.active(root_id)
            .into_iter()
            .map(|strip| now.wall_ms.saturating_add(strip.remaining_ms(now.mono_ms)))
            .min()
    }

    /// Active-first snapshot. Overdue strips are never dropped; the active
    /// window is otherwise capped at `max_active`.
    #[must_use]
    pub fn snapshot(&self, root_id: &str, max_active: usize) -> RegistrySnapshot {
        let now = EngineNow::from_clock(self.clock.as_ref());
        self.snapshot_at(root_id, max_active, now)
    }

    /// [`Self::snapshot`] at an explicit pair of clock readings.
    #[must_use]
    pub fn snapshot_at(
        &self,
        root_id: &str,
        max_active: usize,
        now: EngineNow,
    ) -> RegistrySnapshot {
        let mut active = self.active(root_id);
        active.sort_by_key(|strip| {
            let overdue = !strip.is_overdue(now.mono_ms);
            (overdue, strip.remaining_ms(now.mono_ms), strip.started_ms)
        });
        if active.len() > max_active {
            let mut kept = Vec::with_capacity(max_active.max(1));
            let mut overflow = active.len() - max_active;
            for strip in active {
                if overflow > 0 && !strip.is_overdue(now.mono_ms) {
                    overflow -= 1;
                    continue;
                }
                kept.push(strip);
            }
            active = kept;
        }
        RegistrySnapshot {
            active,
            recent: self.recent(root_id, max_active.max(SNAPSHOT_ACTIVE)),
        }
    }

    /// Look up a strip by tool-call id or session id.
    #[must_use]
    pub fn strip(&self, target: &str) -> Option<FlightStrip> {
        let data = self.data.lock();
        if let Some(strip) = data.strips.get(target) {
            return Some(strip.clone());
        }
        data.strips
            .values()
            .find(|s| s.session_id == target && s.state == StripState::Running)
            .or_else(|| data.strips.values().find(|s| s.session_id == target))
            .cloned()
    }

    /// Number of retained strips (running + settled).
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.lock().strips.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
/// Drop settled strips whose monotonic age exceeds the TTL, then trim the
/// settled index to the cap.
///
/// `now_mono_ms` is a [`crate::clock::Clock::mono_ms`] value, so a wall-clock jump can neither
/// freeze nor accelerate retention. A strip with no monotonic settle stamp (an
/// unstamped fixture) is evicted only by the count cap.
fn prune_root(data: &mut RegistryData, root_id: &str, caps: RegistryCaps, now_mono_ms: i64) {
    let Some(index) = data.roots.get_mut(root_id) else {
        return;
    };
    let mut evicted = Vec::new();
    while let Some(id) = index.settled.back() {
        let expired = data
            .strips
            .get(id)
            .and_then(|strip| strip.settled_mono_ms)
            .is_some_and(|settled| settled.saturating_add(caps.settled_ttl_ms) <= now_mono_ms);
        let over_cap = index.settled.len() > caps.max_settled_per_root;
        if !expired && !over_cap {
            break;
        }
        if let Some(id) = index.settled.pop_back() {
            evicted.push(id);
        }
    }
    for id in evicted {
        data.strips.remove(&id);
    }
}
