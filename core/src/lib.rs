use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Disarmed,
    ExitDelay,
    Armed,
    EntryDelay,
    Triggered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrongStateError {
    pub actual: State,
    pub expected: State,
}

impl fmt::Display for WrongStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "expected state {:?}, but actual state is {:?}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for WrongStateError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneKind {
    Delay,
    Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneStatus {
    Clear,
    Triggered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoneAlreadyExistsError(pub u32);

impl fmt::Display for ZoneAlreadyExistsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "zone {} is already registered", self.0)
    }
}

impl std::error::Error for ZoneAlreadyExistsError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownZoneError(pub u32);

impl fmt::Display for UnknownZoneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "zone {} is not registered", self.0)
    }
}

impl std::error::Error for UnknownZoneError {}

struct Zone {
    kind: ZoneKind,
    status: ZoneStatus,
}

pub struct Alarm {
    state: State,
    zones: HashMap<u32, Zone>,
}

impl Alarm {
    pub fn new() -> Self {
        Alarm {
            state: State::Disarmed,
            zones: HashMap::new(),
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn arm(&mut self) -> Result<(), WrongStateError> {
        if self.state == State::Disarmed {
            self.state = State::ExitDelay;
            Ok(())
        } else {
            Err(WrongStateError {
                actual: self.state,
                expected: State::Disarmed,
            })
        }
    }

    /// Unconditionally transitions to `Disarmed` from any state.
    ///
    /// This deliberately does not reset any zone's status: zone status reflects
    /// physical reality as reported by `report_zone_event`, independent of the
    /// alarm's arming state.
    pub fn disarm(&mut self) {
        self.state = State::Disarmed;
    }

    pub fn add_zone(&mut self, id: u32, kind: ZoneKind) -> Result<(), ZoneAlreadyExistsError> {
        if self.zones.contains_key(&id) {
            return Err(ZoneAlreadyExistsError(id));
        }
        self.zones.insert(
            id,
            Zone {
                kind,
                status: ZoneStatus::Clear,
            },
        );
        Ok(())
    }

    pub fn zone_status(&self, id: u32) -> Option<ZoneStatus> {
        self.zones.get(&id).map(|zone| zone.status)
    }

    pub fn all_zones_clear(&self) -> bool {
        self.zones
            .values()
            .all(|zone| zone.status == ZoneStatus::Clear)
    }

    pub fn report_zone_event(
        &mut self,
        id: u32,
        status: ZoneStatus,
    ) -> Result<(), UnknownZoneError> {
        let zone = self.zones.get_mut(&id).ok_or(UnknownZoneError(id))?;
        zone.status = status;
        let kind = zone.kind;

        match (self.state, status, kind) {
            (State::Armed, ZoneStatus::Triggered, ZoneKind::Instant) => {
                self.state = State::Triggered;
            }
            (State::Armed, ZoneStatus::Triggered, ZoneKind::Delay) => {
                self.state = State::EntryDelay;
            }
            (State::EntryDelay, ZoneStatus::Triggered, ZoneKind::Instant) => {
                self.state = State::Triggered;
            }
            _ => {}
        }

        Ok(())
    }

    pub fn complete_exit_delay(&mut self) -> Result<bool, WrongStateError> {
        if self.state != State::ExitDelay {
            return Err(WrongStateError {
                actual: self.state,
                expected: State::ExitDelay,
            });
        }
        if self.all_zones_clear() {
            self.state = State::Armed;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn complete_entry_delay(&mut self) -> Result<(), WrongStateError> {
        if self.state != State::EntryDelay {
            return Err(WrongStateError {
                actual: self.state,
                expected: State::EntryDelay,
            });
        }
        self.state = State::Triggered;
        Ok(())
    }
}

impl Default for Alarm {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn armed_alarm() -> Alarm {
        let mut alarm = Alarm::new();
        alarm.state = State::Armed;
        alarm
    }

    fn entry_delay_alarm() -> Alarm {
        let mut alarm = Alarm::new();
        alarm.state = State::EntryDelay;
        alarm
    }

    #[test]
    fn initial_state_is_disarmed() {
        let alarm = Alarm::new();
        assert_eq!(alarm.state(), State::Disarmed);
    }

    #[test]
    fn arm_from_disarmed_succeeds() {
        let mut alarm = Alarm::new();
        assert_eq!(alarm.arm(), Ok(()));
        assert_eq!(alarm.state(), State::ExitDelay);
    }

    #[test]
    fn arm_from_exit_delay_fails_and_state_unchanged() {
        let mut alarm = Alarm::new();
        alarm.arm().unwrap();
        assert_eq!(alarm.state(), State::ExitDelay);

        let result = alarm.arm();
        assert_eq!(
            result,
            Err(WrongStateError {
                actual: State::ExitDelay,
                expected: State::Disarmed,
            })
        );
        assert_eq!(alarm.state(), State::ExitDelay);
    }

    #[test]
    fn disarm_from_disarmed_stays_disarmed() {
        let mut alarm = Alarm::new();
        alarm.disarm();
        assert_eq!(alarm.state(), State::Disarmed);
    }

    #[test]
    fn disarm_from_exit_delay_returns_to_disarmed() {
        let mut alarm = Alarm::new();
        alarm.arm().unwrap();
        assert_eq!(alarm.state(), State::ExitDelay);

        alarm.disarm();
        assert_eq!(alarm.state(), State::Disarmed);
    }

    #[test]
    fn add_zone_twice_errors_and_leaves_state_unchanged() {
        let mut alarm = Alarm::new();
        alarm.add_zone(1, ZoneKind::Delay).unwrap();
        let result = alarm.add_zone(1, ZoneKind::Instant);
        assert_eq!(result, Err(ZoneAlreadyExistsError(1)));
        assert_eq!(alarm.zone_status(1), Some(ZoneStatus::Clear));
    }

    #[test]
    fn report_event_unknown_zone_errors() {
        let mut alarm = Alarm::new();
        let result = alarm.report_zone_event(42, ZoneStatus::Triggered);
        assert_eq!(result, Err(UnknownZoneError(42)));
    }

    #[test]
    fn new_zone_starts_clear() {
        let mut alarm = Alarm::new();
        alarm.add_zone(1, ZoneKind::Instant).unwrap();
        assert_eq!(alarm.zone_status(1), Some(ZoneStatus::Clear));
    }

    #[test]
    fn zone_status_unknown_is_none() {
        let alarm = Alarm::new();
        assert_eq!(alarm.zone_status(99), None);
    }

    #[test]
    fn armed_instant_triggered_sets_triggered() {
        let mut alarm = armed_alarm();
        alarm.add_zone(1, ZoneKind::Instant).unwrap();
        alarm.report_zone_event(1, ZoneStatus::Triggered).unwrap();
        assert_eq!(alarm.state(), State::Triggered);
    }

    #[test]
    fn armed_delay_triggered_sets_entry_delay() {
        let mut alarm = armed_alarm();
        alarm.add_zone(1, ZoneKind::Delay).unwrap();
        alarm.report_zone_event(1, ZoneStatus::Triggered).unwrap();
        assert_eq!(alarm.state(), State::EntryDelay);
    }

    #[test]
    fn entry_delay_instant_triggered_sets_triggered() {
        let mut alarm = entry_delay_alarm();
        alarm.add_zone(1, ZoneKind::Instant).unwrap();
        alarm.report_zone_event(1, ZoneStatus::Triggered).unwrap();
        assert_eq!(alarm.state(), State::Triggered);
    }

    #[test]
    fn entry_delay_delay_triggered_stays_entry_delay() {
        let mut alarm = entry_delay_alarm();
        alarm.add_zone(1, ZoneKind::Delay).unwrap();
        alarm.report_zone_event(1, ZoneStatus::Triggered).unwrap();
        assert_eq!(alarm.state(), State::EntryDelay);
    }

    #[test]
    fn exit_delay_zone_event_does_not_change_state() {
        let mut alarm = Alarm::new();
        alarm.arm().unwrap();
        assert_eq!(alarm.state(), State::ExitDelay);
        alarm.add_zone(1, ZoneKind::Instant).unwrap();
        alarm.report_zone_event(1, ZoneStatus::Triggered).unwrap();
        assert_eq!(alarm.state(), State::ExitDelay);
    }

    #[test]
    fn disarmed_zone_event_does_not_change_state() {
        let mut alarm = Alarm::new();
        alarm.add_zone(1, ZoneKind::Instant).unwrap();
        alarm.report_zone_event(1, ZoneStatus::Triggered).unwrap();
        assert_eq!(alarm.state(), State::Disarmed);
    }

    #[test]
    fn triggered_zone_event_does_not_change_state() {
        let mut alarm = Alarm::new();
        alarm.state = State::Triggered;
        alarm.add_zone(1, ZoneKind::Instant).unwrap();
        alarm.report_zone_event(1, ZoneStatus::Triggered).unwrap();
        assert_eq!(alarm.state(), State::Triggered);
    }

    #[test]
    fn all_zones_clear_true_with_no_zones() {
        let alarm = Alarm::new();
        assert!(alarm.all_zones_clear());
    }

    #[test]
    fn all_zones_clear_true_when_all_clear() {
        let mut alarm = Alarm::new();
        alarm.add_zone(1, ZoneKind::Instant).unwrap();
        alarm.add_zone(2, ZoneKind::Delay).unwrap();
        assert!(alarm.all_zones_clear());
    }

    #[test]
    fn all_zones_clear_false_when_one_triggered() {
        let mut alarm = Alarm::new();
        alarm.add_zone(1, ZoneKind::Instant).unwrap();
        alarm.add_zone(2, ZoneKind::Delay).unwrap();
        alarm.report_zone_event(2, ZoneStatus::Triggered).unwrap();
        assert!(!alarm.all_zones_clear());
    }

    #[test]
    fn complete_exit_delay_from_disarmed_errors() {
        let mut alarm = Alarm::new();
        let result = alarm.complete_exit_delay();
        assert_eq!(
            result,
            Err(WrongStateError {
                actual: State::Disarmed,
                expected: State::ExitDelay,
            })
        );
        assert_eq!(alarm.state(), State::Disarmed);
    }

    #[test]
    fn complete_exit_delay_with_no_zones_arms() {
        let mut alarm = Alarm::new();
        alarm.arm().unwrap();
        let result = alarm.complete_exit_delay();
        assert_eq!(result, Ok(true));
        assert_eq!(alarm.state(), State::Armed);
    }

    #[test]
    fn complete_exit_delay_with_triggered_zone_stays_exit_delay() {
        let mut alarm = Alarm::new();
        alarm.add_zone(1, ZoneKind::Instant).unwrap();
        alarm.arm().unwrap();
        alarm.report_zone_event(1, ZoneStatus::Triggered).unwrap();

        let result = alarm.complete_exit_delay();
        assert_eq!(result, Ok(false));
        assert_eq!(alarm.state(), State::ExitDelay);

        alarm.report_zone_event(1, ZoneStatus::Clear).unwrap();
        let result = alarm.complete_exit_delay();
        assert_eq!(result, Ok(true));
        assert_eq!(alarm.state(), State::Armed);
    }

    #[test]
    fn complete_entry_delay_from_other_state_errors() {
        let mut alarm = Alarm::new();
        let result = alarm.complete_entry_delay();
        assert_eq!(
            result,
            Err(WrongStateError {
                actual: State::Disarmed,
                expected: State::EntryDelay,
            })
        );
        assert_eq!(alarm.state(), State::Disarmed);
    }

    #[test]
    fn complete_entry_delay_from_entry_delay_triggers() {
        let mut alarm = entry_delay_alarm();
        let result = alarm.complete_entry_delay();
        assert_eq!(result, Ok(()));
        assert_eq!(alarm.state(), State::Triggered);
    }
}
