use std::sync::Mutex;
use std::time::{Duration, Instant};

const DEFAULT_DELAY_SECS: u64 = 30;

pub fn exit_delay_from_env() -> Duration {
    duration_from_env("TALOS_EXIT_DELAY_SECS")
}

pub fn entry_delay_from_env() -> Duration {
    duration_from_env("TALOS_ENTRY_DELAY_SECS")
}

fn duration_from_env(key: &str) -> Duration {
    let secs = std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_DELAY_SECS);
    Duration::from_secs(secs)
}

#[derive(Default)]
pub struct StateTracker {
    state: Option<talos_core::State>,
    observed_at: Option<Instant>,
}

impl StateTracker {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Advances exit/entry delay timeouts by one check. `now` is taken as a parameter
/// rather than read internally so tests can simulate elapsed time without sleeping.
pub fn check(
    alarm: &Mutex<talos_core::Alarm>,
    tracker: &mut StateTracker,
    exit_delay: Duration,
    entry_delay: Duration,
    now: Instant,
) {
    let mut alarm = alarm.lock().unwrap();
    let state = alarm.state();

    if tracker.state != Some(state) {
        tracker.state = Some(state);
        tracker.observed_at = Some(now);
    }

    let observed_at = tracker
        .observed_at
        .expect("set above on this call or a previous one");

    match state {
        talos_core::State::ExitDelay if now.duration_since(observed_at) >= exit_delay => {
            let _ = alarm.complete_exit_delay();
        }
        talos_core::State::EntryDelay if now.duration_since(observed_at) >= entry_delay => {
            let _ = alarm.complete_entry_delay();
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DELAY: Duration = Duration::from_secs(30);

    #[test]
    fn first_observation_takes_no_action_even_if_now_is_far_past() {
        let alarm = Mutex::new(talos_core::Alarm::new());
        alarm.lock().unwrap().arm().unwrap();
        let mut tracker = StateTracker::new();

        let far_future = Instant::now() + Duration::from_secs(10_000);
        check(&alarm, &mut tracker, DELAY, DELAY, far_future);

        assert_eq!(alarm.lock().unwrap().state(), talos_core::State::ExitDelay);
    }

    #[test]
    fn exit_delay_completes_after_configured_duration_with_no_zones() {
        let alarm = Mutex::new(talos_core::Alarm::new());
        alarm.lock().unwrap().arm().unwrap();
        let mut tracker = StateTracker::new();
        let start = Instant::now();

        check(&alarm, &mut tracker, DELAY, DELAY, start);
        assert_eq!(alarm.lock().unwrap().state(), talos_core::State::ExitDelay);

        check(&alarm, &mut tracker, DELAY, DELAY, start + DELAY);
        assert_eq!(alarm.lock().unwrap().state(), talos_core::State::Armed);
    }

    #[test]
    fn exit_delay_waits_for_zone_clear_before_completing() {
        let alarm = Mutex::new(talos_core::Alarm::new());
        {
            let mut guard = alarm.lock().unwrap();
            guard.add_zone(1, talos_core::ZoneKind::Instant).unwrap();
            guard.arm().unwrap();
            guard
                .report_zone_event(1, talos_core::ZoneStatus::Triggered)
                .unwrap();
        }
        let mut tracker = StateTracker::new();
        let start = Instant::now();

        check(&alarm, &mut tracker, DELAY, DELAY, start);
        check(&alarm, &mut tracker, DELAY, DELAY, start + DELAY);
        assert_eq!(alarm.lock().unwrap().state(), talos_core::State::ExitDelay);

        alarm
            .lock()
            .unwrap()
            .report_zone_event(1, talos_core::ZoneStatus::Clear)
            .unwrap();

        check(
            &alarm,
            &mut tracker,
            DELAY,
            DELAY,
            start + DELAY + Duration::from_secs(1),
        );
        assert_eq!(alarm.lock().unwrap().state(), talos_core::State::Armed);
    }

    #[test]
    fn entry_delay_completes_after_configured_duration() {
        let alarm = Mutex::new(talos_core::Alarm::new());
        {
            let mut guard = alarm.lock().unwrap();
            guard.add_zone(1, talos_core::ZoneKind::Delay).unwrap();
            guard.arm().unwrap();
            guard.complete_exit_delay().unwrap();
            guard
                .report_zone_event(1, talos_core::ZoneStatus::Triggered)
                .unwrap();
        }
        assert_eq!(alarm.lock().unwrap().state(), talos_core::State::EntryDelay);

        let mut tracker = StateTracker::new();
        let start = Instant::now();

        check(&alarm, &mut tracker, DELAY, DELAY, start);
        assert_eq!(alarm.lock().unwrap().state(), talos_core::State::EntryDelay);

        check(&alarm, &mut tracker, DELAY, DELAY, start + DELAY);
        assert_eq!(alarm.lock().unwrap().state(), talos_core::State::Triggered);
    }
}
