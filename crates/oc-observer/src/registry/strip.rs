//! The flight strip: the only per-tool-call record the observer sees.

use serde::{Deserialize, Serialize};

use super::caps::MS_PER_SECOND;

/// Lifecycle state of a flight strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StripState {
    /// The tool call is still executing.
    #[default]
    Running,
    /// Completed successfully.
    Ok,
    /// Completed with an error.
    Error,
    /// Cancelled by the host, the user or a repair.
    Cancelled,
    /// Exceeded its intervention deadline and was stopped.
    TimedOut,
    /// Re-run by an observer repair.
    Repaired,
}

impl StripState {
    /// Whether the strip has finished.
    #[must_use]
    pub fn is_settled(self) -> bool {
        !matches!(self, Self::Running)
    }

    /// Whether this settlement is a failure worth waking the observer for.
    #[must_use]
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Error | Self::Cancelled | Self::TimedOut)
    }

    /// Wire name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Repaired => "repaired",
        }
    }
}
/// One tool call as the observer sees it.
///
/// `why` is the only free text. `retryable` is host metadata (idempotent tool
/// or explicit `allow_retry`) and is never rendered into the model input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FlightStrip {
    /// Observation id (`obs_…`).
    pub id: String,
    /// Root session that owns the call.
    pub root_id: String,
    /// Session that issued the call.
    pub session_id: String,
    /// Agent id.
    pub agent: String,
    /// Provider tool-call id.
    pub tool_call_id: String,
    /// Tool name.
    pub tool: String,
    /// One sentence: action + expected outcome.
    pub why: String,
    /// Agent estimate, seconds.
    pub expect_s: u32,
    /// Observer intervention deadline, seconds.
    pub intervene_s: u32,
    /// Open time, epoch ms (wall clock; published and audited).
    pub started_ms: i64,
    /// Settle time, epoch ms (wall clock; published and audited).
    pub settled_ms: Option<i64>,
    /// Open time on the process-monotonic clock.
    ///
    /// `None` means "unstamped" (a fixture or a strip opened by a producer that
    /// predates this field). Deadline logic prefers this stamp and falls back
    /// to the wall-clock path when it is absent.
    #[serde(skip)]
    pub started_mono_ms: Option<i64>,
    /// Settle time on the process-monotonic clock.
    ///
    /// Used for the retention TTL and for settled elapsed time, so neither is
    /// affected by a wall-clock jump.
    #[serde(skip)]
    pub settled_mono_ms: Option<i64>,
    /// Lifecycle state.
    pub state: StripState,
    /// One-line result, capped at [`super::caps::MAX_OUTCOME_CHARS`].
    pub outcome: Option<String>,
    /// Parent tool call (subagent nesting).
    pub parent_call: Option<String>,
    /// Session depth.
    pub depth: u32,
    /// Host metadata: may this tool call be retried by a repair?
    pub retryable: bool,
    /// Host metadata: the call this strip retries.
    pub retry_of: Option<String>,
}

impl Default for FlightStrip {
    fn default() -> Self {
        Self {
            id: String::new(),
            root_id: String::new(),
            session_id: String::new(),
            agent: String::new(),
            tool_call_id: String::new(),
            tool: String::new(),
            why: String::new(),
            expect_s: 0,
            intervene_s: 0,
            started_ms: 0,
            settled_ms: None,
            started_mono_ms: None,
            settled_mono_ms: None,
            state: StripState::Running,
            outcome: None,
            parent_call: None,
            depth: 0,
            retryable: false,
            retry_of: None,
        }
    }
}

impl FlightStrip {
    /// Wall-clock deadline at which the observer may intervene.
    ///
    /// This is the published/audited instant and keeps its exact previous
    /// meaning. Deadline *decisions* use [`Self::deadline_mono_ms`], which a
    /// wall-clock jump cannot move.
    #[must_use]
    pub fn deadline_ms(&self) -> i64 {
        self.started_ms
            .saturating_add(i64::from(self.intervene_s).saturating_mul(MS_PER_SECOND))
    }

    /// Intervention window in milliseconds.
    #[must_use]
    pub fn intervene_ms(&self) -> i64 {
        i64::from(self.intervene_s).saturating_mul(MS_PER_SECOND)
    }

    /// Monotonic deadline, when the strip carries a monotonic start stamp.
    #[must_use]
    pub fn deadline_mono_ms(&self) -> Option<i64> {
        self.started_mono_ms
            .map(|started| started.saturating_add(self.intervene_ms()))
    }

    /// Elapsed milliseconds on the monotonic clock, clamped at zero.
    ///
    /// Both monotonic stamps present: a pure monotonic duration. Only the start
    /// stamp present: the running duration. No monotonic start stamp at all (a
    /// fixture, or a producer that predates the field): the wall-clock duration
    /// when the strip is settled, otherwise **zero** — an unstamped running
    /// strip yields no deadline evidence rather than comparing a wall origin
    /// against a monotonic now.
    #[must_use]
    pub fn elapsed_mono_ms(&self, now_mono_ms: i64) -> i64 {
        match (self.started_mono_ms, self.settled_mono_ms) {
            (Some(started), Some(settled)) => settled.saturating_sub(started).max(0),
            (Some(started), None) => now_mono_ms.saturating_sub(started).max(0),
            (None, _) => self
                .settled_ms
                .map_or(0, |settled| settled.saturating_sub(self.started_ms).max(0)),
        }
    }

    /// Milliseconds left before the intervention deadline, clamped at zero.
    ///
    /// Derived from the monotonic elapsed time, so a wall-clock jump cannot
    /// move it.
    #[must_use]
    pub fn remaining_ms(&self, now_mono_ms: i64) -> i64 {
        self.intervene_ms()
            .saturating_sub(self.elapsed_mono_ms(now_mono_ms))
            .max(0)
    }

    /// True when the call is still running past its intervention deadline.
    ///
    /// `now_mono_ms` is a [`crate::clock::Clock::mono_ms`] value, never a wall clock.
    #[must_use]
    pub fn is_overdue(&self, now_mono_ms: i64) -> bool {
        self.state == StripState::Running
            && self.elapsed_mono_ms(now_mono_ms) >= self.intervene_ms()
    }

    /// Elapsed seconds, clamped at zero, on the monotonic clock.
    ///
    /// A wall-clock jump cannot change this value.
    #[must_use]
    pub fn elapsed_s(&self, now_mono_ms: i64) -> i64 {
        self.elapsed_mono_ms(now_mono_ms) / MS_PER_SECOND
    }
}
