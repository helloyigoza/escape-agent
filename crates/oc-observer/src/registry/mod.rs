//! The expectation registry: one terse [`FlightStrip`] per tool call.
//!
//! Only strips are kept. Tool arguments, outputs and assistant text never
//! enter this module. Settled strips live long enough for the observer and the
//! UI to reference them, then are pruned.
//!
//! Growth caps (per root):
//! - settled strips are dropped [`SETTLED_TTL_MS`] after settling;
//! - at most [`MAX_SETTLED_PER_ROOT`] settled strips are retained (oldest
//!   dropped first);
//! - running strips are bounded by actual in-flight tool calls.
//!
//! Both caps are documented in the crate docs and enforced on every mutation.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::clock::Clock;

mod caps;
mod store;
mod strip;

pub use caps::{
    MAX_OUTCOME_CHARS, MAX_SETTLED_PER_ROOT, MAX_WHY_CHARS, MS_PER_SECOND, RegistryCaps,
    RegistrySnapshot, SETTLED_TTL_MS, SNAPSHOT_ACTIVE,
};
pub use strip::{FlightStrip, StripState};

use store::RegistryData;

/// Per-root expectation registry.
///
/// The registry owns a [`Clock`] so settlement timestamps and TTL pruning are
/// deterministic in tests.
pub struct ExpectationRegistry {
    clock: Arc<dyn Clock>,
    caps: RegistryCaps,
    data: Mutex<RegistryData>,
}
impl std::fmt::Debug for ExpectationRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExpectationRegistry")
            .field("caps", &self.caps)
            .field("strips", &self.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    use crate::clock::EngineNow;

    /// Wall and monotonic readings held separately, so a test can move one
    /// clock without moving the other.
    #[derive(Debug, Default)]
    struct FakeClock {
        wall: AtomicI64,
        mono: AtomicI64,
    }

    impl FakeClock {
        /// Move both clocks together (the common case).
        fn set(&self, value: i64) {
            self.wall.store(value, Ordering::SeqCst);
            self.mono.store(value, Ordering::SeqCst);
        }

        /// Move only the wall clock (an NTP correction).
        fn set_wall(&self, value: i64) {
            self.wall.store(value, Ordering::SeqCst);
        }

        /// Move only the monotonic clock (real elapsed time).
        fn set_mono(&self, value: i64) {
            self.mono.store(value, Ordering::SeqCst);
        }
    }

    impl Clock for FakeClock {
        fn now_ms(&self) -> i64 {
            self.wall.load(Ordering::SeqCst)
        }

        fn mono_ms(&self) -> i64 {
            self.mono.load(Ordering::SeqCst)
        }
    }

    fn strip(id: &str, root: &str, session: &str, started: i64, intervene: u32) -> FlightStrip {
        FlightStrip {
            id: format!("obs_{id}"),
            root_id: root.to_owned(),
            session_id: session.to_owned(),
            agent: "build".to_owned(),
            tool_call_id: id.to_owned(),
            tool: "shell".to_owned(),
            why: "run focused test".to_owned(),
            expect_s: 10,
            intervene_s: intervene,
            started_ms: started,
            // The helper stamps both clocks so a strip is never "unstamped".
            started_mono_ms: Some(started),
            retryable: true,
            ..FlightStrip::default()
        }
    }

    #[test]
    fn lifecycle_and_overdue() {
        let clock = Arc::new(FakeClock::default());
        clock.set(1_000);
        let registry = ExpectationRegistry::new(clock.clone());
        registry.open(strip("tc_1", "ses_root", "ses_root", 1_000, 10));

        assert_eq!(registry.active("ses_root").len(), 1);
        assert_eq!(registry.earliest_deadline("ses_root"), Some(11_000));

        clock.set(10_999);
        assert!(!registry.active("ses_root")[0].is_overdue(clock.now_ms()));
        clock.set(11_000);
        assert!(registry.active("ses_root")[0].is_overdue(clock.now_ms()));

        let long_outcome = "x".repeat(200);
        assert!(registry.settle("tc_1", StripState::Ok, Some(&long_outcome)));
        let settled = &registry.recent("ses_root", 1)[0];
        assert_eq!(settled.state, StripState::Ok);
        assert_eq!(settled.settled_ms, Some(11_000));
        let outcome = settled.outcome.as_deref().unwrap_or_default();
        assert_eq!(outcome.chars().count(), MAX_OUTCOME_CHARS);
        assert!(registry.active("ses_root").is_empty());
        assert!(!registry.settle("tc_missing", StripState::Ok, None));

        registry.open(strip("tc_2", "ses_root", "ses_child", 11_000, 10));
        assert_eq!(registry.cancel_session("ses_child"), 1);
        assert_eq!(registry.active("ses_root").len(), 0);
        assert_eq!(registry.recent("ses_root", 5)[0].tool_call_id, "tc_2");
    }

    #[test]
    fn settled_strips_are_pruned() {
        let clock = Arc::new(FakeClock::default());
        clock.set(0);
        let registry = ExpectationRegistry::with_caps(
            clock.clone(),
            RegistryCaps {
                max_settled_per_root: 2,
                settled_ttl_ms: SETTLED_TTL_MS,
            },
        );
        for index in 0..4 {
            let id = format!("tc_{index}");
            registry.open(strip(&id, "ses_root", "ses_root", 0, 10));
            registry.settle(&id, StripState::Ok, Some("ok"));
        }
        assert_eq!(registry.recent("ses_root", 10).len(), 2);

        clock.set(SETTLED_TTL_MS + 1);
        registry.open(strip("tc_new", "ses_root", "ses_root", clock.now_ms(), 10));
        assert_eq!(registry.recent("ses_root", 10).len(), 0);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn snapshot_caps_active_but_keeps_overdue() {
        let clock = Arc::new(FakeClock::default());
        clock.set(100_000);
        let registry = ExpectationRegistry::new(clock);
        for index in 0..30 {
            let started = if index < 2 { 0 } else { 99_000 };
            let id = format!("tc_{index}");
            registry.open(strip(&id, "ses_root", "ses_root", started, 10));
        }
        let snapshot = registry.snapshot("ses_root", SNAPSHOT_ACTIVE);
        assert_eq!(snapshot.active.len(), SNAPSHOT_ACTIVE);
        let overdue: Vec<_> = snapshot
            .active
            .iter()
            .filter(|strip| strip.is_overdue(100_000))
            .collect();
        assert_eq!(overdue.len(), 2);
        assert_eq!(
            snapshot.active[0].tool_call_id, "tc_0",
            "overdue strips sort first"
        );
    }

    // -----------------------------------------------------------------------
    // T8: deadlines run on the monotonic clock, wall time is only published
    // -----------------------------------------------------------------------

    #[test]
    fn forward_wall_jump_does_not_make_running_strips_overdue() {
        let clock = Arc::new(FakeClock::default());
        clock.set(1_000);
        let registry = ExpectationRegistry::new(clock.clone());
        registry.open(strip("tc_1", "ses_root", "ses_root", 1_000, 10));

        // One hour of NTP correction forwards; no real time passed.
        clock.set_wall(1_000 + 3_600_000);

        let now = EngineNow::from_clock(clock.as_ref());
        let strip = &registry.active("ses_root")[0];
        assert!(
            !strip.is_overdue(now.mono_ms),
            "a forward wall jump must not fire every overdue strip at once"
        );
        assert_eq!(strip.elapsed_s(now.mono_ms), 0, "no real time elapsed");
        // 10 s of the window is still unused, so the deadline is 10 s after the
        // *new* wall instant: an absolute wall time that the jump moved with it,
        // while the decision itself stayed monotonic.
        assert_eq!(registry.earliest_deadline("ses_root"), Some(3_611_000));
    }

    #[test]
    fn backward_wall_jump_does_not_suppress_a_genuinely_overdue_strip() {
        let clock = Arc::new(FakeClock::default());
        clock.set(1_000_000);
        let registry = ExpectationRegistry::new(clock.clone());
        registry.open(strip("tc_1", "ses_root", "ses_root", 1_000_000, 10));

        // 20 s of real elapsed time, then a backward wall jump of one hour.
        clock.set_mono(1_020_000);
        clock.set_wall(1_000_000 - 3_600_000);

        let now = EngineNow::from_clock(clock.as_ref());
        let strip = &registry.active("ses_root")[0];
        assert!(
            strip.is_overdue(now.mono_ms),
            "a backward wall jump must not hide a real deadline breach"
        );
        assert_eq!(strip.elapsed_s(now.mono_ms), 20);
        assert_eq!(strip.remaining_ms(now.mono_ms), 0);
        assert_eq!(
            registry.earliest_deadline("ses_root"),
            Some(now.wall_ms),
            "no time is left, so the earliest deadline is now"
        );
    }

    #[test]
    fn elapsed_s_is_immune_to_wall_clock_movement() {
        let clock = Arc::new(FakeClock::default());
        clock.set(50_000);
        let registry = ExpectationRegistry::new(clock.clone());
        registry.open(strip("tc_1", "ses_root", "ses_root", 50_000, 600));

        clock.set_mono(65_000);
        let before = registry.active("ses_root")[0].elapsed_s(clock.mono_ms());
        assert_eq!(before, 15);

        clock.set_wall(50_000 - 9_000_000);
        assert_eq!(
            registry.active("ses_root")[0].elapsed_s(clock.mono_ms()),
            before,
            "elapsed time must not follow the wall clock backwards"
        );

        clock.set_wall(50_000 + 9_000_000);
        assert_eq!(
            registry.active("ses_root")[0].elapsed_s(clock.mono_ms()),
            before,
            "elapsed time must not follow the wall clock forwards"
        );
    }

    #[test]
    fn a_real_monotonic_breach_is_overdue() {
        let clock = Arc::new(FakeClock::default());
        clock.set(7_000);
        let registry = ExpectationRegistry::new(clock.clone());
        registry.open(strip("tc_1", "ses_root", "ses_root", 7_000, 5));

        clock.set_mono(7_000 + 4_999);
        let now = EngineNow::from_clock(clock.as_ref());
        assert!(!registry.active("ses_root")[0].is_overdue(now.mono_ms));

        clock.set_mono(7_000 + 5_000);
        let now = EngineNow::from_clock(clock.as_ref());
        assert!(
            registry.active("ses_root")[0].is_overdue(now.mono_ms),
            "a genuine intervene_s breach must be overdue"
        );
    }

    #[test]
    fn pruning_follows_the_monotonic_ttl_not_the_wall_clock() {
        let clock = Arc::new(FakeClock::default());
        clock.set(0);
        let registry = ExpectationRegistry::with_caps(
            clock.clone(),
            RegistryCaps {
                max_settled_per_root: MAX_SETTLED_PER_ROOT,
                settled_ttl_ms: SETTLED_TTL_MS,
            },
        );
        registry.open(strip("tc_1", "ses_root", "ses_root", 0, 10));
        registry.settle("tc_1", StripState::Ok, Some("ok"));
        assert_eq!(registry.recent("ses_root", 10).len(), 1);

        // A backward wall jump must not freeze the TTL.
        clock.set_wall(-9_000_000);
        clock.set_mono(SETTLED_TTL_MS + 1);
        registry.open(strip("tc_2", "ses_root", "ses_root", 0, 10));
        assert_eq!(
            registry.recent("ses_root", 10).len(),
            0,
            "the monotonic TTL must still evict after a backward wall jump"
        );
    }

    #[test]
    fn an_unstamped_strip_is_not_treated_as_ancient() {
        // A fixture (or a producer predating the monotonic field) has no start
        // stamp. 0 is not a real origin, so it must read as freshly opened
        // rather than as "started at process boot".
        let mut fixture = FlightStrip {
            state: StripState::Running,
            intervene_s: 10,
            started_ms: 0,
            ..FlightStrip::default()
        };
        assert_eq!(
            fixture.elapsed_mono_ms(9_000_000),
            0,
            "an unstamped running strip has no measurable monotonic age"
        );
        assert!(
            !fixture.is_overdue(9_000_000),
            "an unstamped strip must never look overdue"
        );
        assert_eq!(fixture.remaining_ms(9_000_000), 10_000);

        // A settled unstamped strip still reports its wall-clock duration.
        fixture.state = StripState::Ok;
        fixture.started_ms = 1_000;
        fixture.settled_ms = Some(6_000);
        assert_eq!(fixture.elapsed_mono_ms(9_000_000), 5_000);
    }
}
