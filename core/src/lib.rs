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
pub struct NotDisarmedError(pub State);

impl fmt::Display for NotDisarmedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot arm: current state is {:?}, not Disarmed", self.0)
    }
}

impl std::error::Error for NotDisarmedError {}

pub struct Alarm {
    state: State,
}

impl Alarm {
    pub fn new() -> Self {
        Alarm {
            state: State::Disarmed,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn arm(&mut self) -> Result<(), NotDisarmedError> {
        if self.state == State::Disarmed {
            self.state = State::ExitDelay;
            Ok(())
        } else {
            Err(NotDisarmedError(self.state))
        }
    }

    pub fn disarm(&mut self) {
        self.state = State::Disarmed;
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
        assert_eq!(result, Err(NotDisarmedError(State::ExitDelay)));
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
}
