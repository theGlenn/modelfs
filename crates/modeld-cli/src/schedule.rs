//! Decides when the daemon runs its next pass.
//!
//! Three triggers, earliest wins: a burst of filesystem events once it goes
//! quiet (capped so a steady stream, like a long download, cannot starve
//! passes), a retry for files deferred until they settle, and a periodic
//! reconcile that catches missed events and newly created scan roots.
//! Pure state over `Instant`s so the policy is testable without a clock.

use std::time::{Duration, Instant};

/// Knobs for [`Schedule`].
#[derive(Debug, Clone, Copy)]
pub struct Timings {
    /// Run a pass once filesystem events have been quiet this long.
    pub quiet: Duration,
    /// Never postpone a pass longer than this after the first unhandled event.
    pub max_delay: Duration,
    /// Reconcile at least this often, events or not.
    pub periodic: Duration,
}

/// Pass scheduling state for the daemon.
#[derive(Debug)]
pub struct Schedule {
    timings: Timings,
    burst: Option<Burst>,
    retry_at: Option<Instant>,
    last_pass: Instant,
}

/// Unhandled filesystem events since the last pass.
#[derive(Debug, Clone, Copy)]
struct Burst {
    first: Instant,
    last: Instant,
}

impl Schedule {
    /// Starts a schedule whose first pass is due at `now`.
    pub fn new(now: Instant, timings: Timings) -> Self {
        Self {
            timings,
            burst: None,
            retry_at: Some(now),
            last_pass: now,
        }
    }

    /// Whether a pass should start at `now`.
    pub fn is_due(&self, now: Instant) -> bool {
        now >= self.next_pass_at()
    }

    /// How long to wait before the next pass is due (zero if already due).
    pub fn time_until_due(&self, now: Instant) -> Duration {
        self.next_pass_at().saturating_duration_since(now)
    }

    /// Notes a relevant filesystem event observed at `now`.
    pub fn record_event(&mut self, now: Instant) {
        self.burst = Some(match self.burst {
            Some(burst) => Burst { last: now, ..burst },
            None => Burst {
                first: now,
                last: now,
            },
        });
    }

    /// Notes a completed pass; `retry_in` asks for another pass that soon.
    ///
    /// Events recorded before this call count as handled by the pass.
    pub fn pass_finished(&mut self, now: Instant, retry_in: Option<Duration>) {
        self.burst = None;
        self.retry_at = retry_in.map(|delay| now + delay);
        self.last_pass = now;
    }

    fn next_pass_at(&self) -> Instant {
        let periodic = self.last_pass + self.timings.periodic;
        let debounced = self.burst.map(|burst| {
            (burst.last + self.timings.quiet).min(burst.first + self.timings.max_delay)
        });
        [Some(periodic), debounced, self.retry_at]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(periodic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMINGS: Timings = Timings {
        quiet: Duration::from_secs(10),
        max_delay: Duration::from_mins(2),
        periodic: Duration::from_mins(15),
    };

    fn after_first_pass() -> (Instant, Schedule) {
        let start = Instant::now();
        let mut schedule = Schedule::new(start, TIMINGS);
        schedule.pass_finished(start, None);
        (start, schedule)
    }

    #[test]
    fn first_pass_is_due_immediately() {
        let now = Instant::now();
        assert!(Schedule::new(now, TIMINGS).is_due(now));
    }

    #[test]
    fn idle_schedule_waits_for_the_periodic_reconcile() {
        let (start, schedule) = after_first_pass();

        assert!(!schedule.is_due(start + Duration::from_secs(899)));
        assert!(schedule.is_due(start + TIMINGS.periodic));
        assert_eq!(schedule.time_until_due(start), TIMINGS.periodic);
    }

    #[test]
    fn event_runs_a_pass_once_quiet() {
        let (start, mut schedule) = after_first_pass();
        let event = start + Duration::from_secs(30);

        schedule.record_event(event);

        assert!(!schedule.is_due(event + Duration::from_secs(9)));
        assert!(schedule.is_due(event + TIMINGS.quiet));
    }

    #[test]
    fn steady_events_cannot_postpone_a_pass_past_max_delay() {
        let (start, mut schedule) = after_first_pass();
        let mut now = start;
        for _ in 0..30 {
            now += Duration::from_secs(5);
            schedule.record_event(now);
        }

        assert!(schedule.is_due(start + Duration::from_secs(5) + TIMINGS.max_delay));
    }

    #[test]
    fn settle_retry_brings_the_next_pass_forward() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start, TIMINGS);

        schedule.pass_finished(start, Some(Duration::from_mins(4)));

        assert!(!schedule.is_due(start + Duration::from_secs(239)));
        assert!(schedule.is_due(start + Duration::from_mins(4)));
    }

    #[test]
    fn finishing_a_pass_clears_handled_events() {
        let (start, mut schedule) = after_first_pass();
        schedule.record_event(start);
        let done = start + TIMINGS.quiet;

        schedule.pass_finished(done, None);

        assert!(!schedule.is_due(done + TIMINGS.quiet));
        assert_eq!(
            schedule.time_until_due(done + TIMINGS.periodic),
            Duration::ZERO
        );
    }
}
