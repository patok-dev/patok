//! The discovery cooldown schedule.
//!
//! A pure state machine over `Instant` values: the engine passes the current time in, so
//! the unit tests drive it with a fake clock instead of sleeping out real cooldowns.

use std::time::{Duration, Instant};

/// When the next discovery round may run, and how the cooldown between rounds evolves.
///
/// - A fresh schedule is eligible immediately; nothing has postponed it yet.
/// - Completing a UI-added task resets the effective cooldown to the configured value and
///   postpones the next round by it.
/// - A round that adds nothing doubles the effective cooldown, capped; every finished round
///   postpones the next one by one effective cooldown.
#[derive(Clone, Debug)]
pub struct DiscoverySchedule {
    /// The configured cooldown a UI-added completion resets to.
    default: Duration,
    /// The largest the doubled cooldown grows to.
    cap: Duration,
    /// The cooldown currently in effect.
    effective: Duration,
    /// The earliest time the next round may run.
    next_eligible: Instant,
}

impl DiscoverySchedule {
    pub fn new(default: Duration, cap: Duration, now: Instant) -> Self {
        Self {
            default,
            cap,
            effective: default,
            next_eligible: now,
        }
    }

    /// Whether a discovery round may run at `now`.
    pub fn is_eligible(&self, now: Instant) -> bool {
        now >= self.next_eligible
    }

    /// A UI-added task completed: the effective cooldown resets to
    /// the configured value and the next round is postponed by it, so the user has time to
    /// add more tasks.
    pub fn ui_added_task_completed(&mut self, now: Instant) {
        self.effective = self.default;
        self.postpone(now);
    }

    /// A discovery round finished having appended `added` tasks. A round that adds nothing
    /// doubles the effective cooldown up to the cap; every round postpones the next one by
    /// the effective cooldown.
    pub fn round_finished(&mut self, now: Instant, added: usize) {
        if added == 0 {
            self.effective = self.effective.saturating_mul(2).min(self.cap);
        }
        self.postpone(now);
    }

    /// New configured limits arrived (a settings change or config reload): the default and
    /// cap are updated and the cooldown currently in effect is clamped to the new cap, so
    /// a smaller limit applies to the next round. `next_eligible` is left alone: a round
    /// already postponed stays postponed.
    pub fn reconfigure(&mut self, default: Duration, cap: Duration) {
        self.default = default;
        self.cap = cap;
        self.effective = self.effective.min(cap);
    }

    fn postpone(&mut self, now: Instant) {
        self.next_eligible = now + self.effective;
    }

    /// The cooldown currently in effect.
    #[cfg(test)]
    pub fn effective_cooldown(&self) -> Duration {
        self.effective
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COOLDOWN: Duration = Duration::from_secs(5 * 60);
    const CAP: Duration = Duration::from_secs(30 * 60);

    /// A fake clock the schedule is driven with: tests advance it instead of waiting.
    struct FakeClock {
        now: Instant,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: Instant::now(),
            }
        }

        fn advance(&mut self, by: Duration) {
            self.now += by;
        }

        /// Steps just past one cooldown, the usual "wait the cooldown out" move.
        fn advance_cooldown(&mut self, schedule: &DiscoverySchedule) {
            self.now += schedule.effective_cooldown();
        }
    }

    #[test]
    fn a_fresh_schedule_is_eligible_immediately() {
        let mut clock = FakeClock::new();
        let schedule = DiscoverySchedule::new(COOLDOWN, CAP, clock.now);
        assert!(schedule.is_eligible(clock.now));
        clock.advance(Duration::from_secs(1));
        assert!(schedule.is_eligible(clock.now));
    }

    #[test]
    fn a_ui_added_completion_postpones_by_the_cooldown() {
        let mut clock = FakeClock::new();
        let mut schedule = DiscoverySchedule::new(COOLDOWN, CAP, clock.now);
        schedule.ui_added_task_completed(clock.now);
        assert!(!schedule.is_eligible(clock.now));
        clock.advance(COOLDOWN - Duration::from_secs(1));
        assert!(!schedule.is_eligible(clock.now));
        clock.advance(Duration::from_secs(1));
        assert!(schedule.is_eligible(clock.now));
    }

    #[test]
    fn a_round_that_adds_nothing_doubles_the_cooldown_up_to_the_cap() {
        let mut clock = FakeClock::new();
        let mut schedule = DiscoverySchedule::new(COOLDOWN, CAP, clock.now);
        // 5 min -> 10 -> 20 -> 40 capped at 30 -> 60 capped at 30.
        for expected in [
            Duration::from_secs(10 * 60),
            Duration::from_secs(20 * 60),
            Duration::from_secs(30 * 60),
            Duration::from_secs(30 * 60),
        ] {
            schedule.round_finished(clock.now, 0);
            assert_eq!(schedule.effective_cooldown(), expected);
            // The next round waits out exactly the doubled cooldown.
            clock.advance(expected - Duration::from_secs(1));
            assert!(!schedule.is_eligible(clock.now));
            clock.advance(Duration::from_secs(1));
            assert!(schedule.is_eligible(clock.now));
        }
    }

    #[test]
    fn a_round_with_tasks_keeps_the_cooldown() {
        let mut clock = FakeClock::new();
        let mut schedule = DiscoverySchedule::new(COOLDOWN, CAP, clock.now);
        schedule.round_finished(clock.now, 2);
        assert_eq!(schedule.effective_cooldown(), COOLDOWN);
        clock.advance(COOLDOWN - Duration::from_secs(1));
        assert!(!schedule.is_eligible(clock.now));
        clock.advance(Duration::from_secs(1));
        assert!(schedule.is_eligible(clock.now));
    }

    #[test]
    fn another_ui_added_completion_resets_a_doubled_cooldown() {
        let mut clock = FakeClock::new();
        let mut schedule = DiscoverySchedule::new(COOLDOWN, CAP, clock.now);
        schedule.round_finished(clock.now, 0);
        schedule.round_finished(clock.now, 0);
        assert_eq!(schedule.effective_cooldown(), Duration::from_secs(20 * 60));
        schedule.ui_added_task_completed(clock.now);
        assert_eq!(schedule.effective_cooldown(), COOLDOWN);
        clock.advance_cooldown(&schedule);
        assert!(schedule.is_eligible(clock.now));
    }

    #[test]
    fn configured_limits_are_honoured() {
        let clock = FakeClock::new();
        let mut schedule =
            DiscoverySchedule::new(Duration::from_secs(10), Duration::from_secs(30), clock.now);
        schedule.round_finished(clock.now, 0);
        assert_eq!(schedule.effective_cooldown(), Duration::from_secs(20));
        schedule.round_finished(clock.now, 0);
        // 40 s doubles up to the 30 s cap.
        assert_eq!(schedule.effective_cooldown(), Duration::from_secs(30));
    }

    #[test]
    fn reconfigure_clamps_the_effective_cooldown_and_keeps_the_postponement() {
        let mut clock = FakeClock::new();
        let mut schedule = DiscoverySchedule::new(COOLDOWN, CAP, clock.now);
        // A doubled cooldown is in effect and the next round is postponed.
        schedule.round_finished(clock.now, 0);
        assert_eq!(schedule.effective_cooldown(), Duration::from_secs(10 * 60));
        assert!(!schedule.is_eligible(clock.now));

        // A shorter configured cap applies to the cooldown in effect, but a round
        // already postponed stays postponed by the old cooldown.
        schedule.reconfigure(Duration::from_secs(60), Duration::from_secs(120));
        assert_eq!(schedule.effective_cooldown(), Duration::from_secs(120));
        assert!(!schedule.is_eligible(clock.now));
        clock.advance(Duration::from_secs(10 * 60));
        assert!(schedule.is_eligible(clock.now));

        // From then on the new limits govern: the default resets on a UI-added
        // completion, and doubling stops at the new cap.
        schedule.ui_added_task_completed(clock.now);
        assert_eq!(schedule.effective_cooldown(), Duration::from_secs(60));
        clock.advance(Duration::from_secs(60));
        assert!(schedule.is_eligible(clock.now));
        schedule.round_finished(clock.now, 0);
        assert_eq!(schedule.effective_cooldown(), Duration::from_secs(120));
    }
}
