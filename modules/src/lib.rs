#[cfg(feature = "shelly")]
pub mod shelly;
#[cfg(feature = "sia_dc09")]
pub mod sia_dc09;

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    Triggered,
    Normal,
}

pub trait Actionneur {
    fn on_state_change(
        &mut self,
        state: talos_core::State,
        zones: &[(u32, talos_core::ZoneKind, talos_core::ZoneStatus)],
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportError {
    UnknownSensor(String),
    UnknownZone(u32),
}

impl fmt::Display for ReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReportError::UnknownSensor(sensor_id) => {
                write!(f, "sensor {sensor_id} is not mapped to a zone")
            }
            ReportError::UnknownZone(zone_id) => write!(f, "zone {zone_id} is not registered"),
        }
    }
}

impl std::error::Error for ReportError {}

/// Bridges sensor reports (e.g. from a Shelly gateway webhook, not yet wired
/// up) to the shared alarm state, mirroring the state-change notification
/// done at the other call sites in `routes.rs`/`timers.rs`.
pub struct AlarmHandle {
    alarm: Arc<Mutex<talos_core::Alarm>>,
    tx: tokio::sync::broadcast::Sender<talos_core::State>,
    actioneurs: Arc<Mutex<Vec<Box<dyn Actionneur + Send>>>>,
    sensor_to_zone: HashMap<String, u32>,
}

impl AlarmHandle {
    pub fn new(
        alarm: Arc<Mutex<talos_core::Alarm>>,
        tx: tokio::sync::broadcast::Sender<talos_core::State>,
        actioneurs: Arc<Mutex<Vec<Box<dyn Actionneur + Send>>>>,
        sensor_to_zone: HashMap<String, u32>,
    ) -> Self {
        AlarmHandle {
            alarm,
            tx,
            actioneurs,
            sensor_to_zone,
        }
    }

    pub fn report(&self, sensor_id: &str, reading: Reading) -> Result<(), ReportError> {
        let zone_id = *self
            .sensor_to_zone
            .get(sensor_id)
            .ok_or_else(|| ReportError::UnknownSensor(sensor_id.to_string()))?;

        let status = match reading {
            Reading::Triggered => talos_core::ZoneStatus::Triggered,
            Reading::Normal => talos_core::ZoneStatus::Clear,
        };

        let mut alarm = self.alarm.lock().unwrap();
        let state_before = alarm.state();
        alarm
            .report_zone_event(zone_id, status)
            .map_err(|talos_core::UnknownZoneError(id)| ReportError::UnknownZone(id))?;
        let state_after = alarm.state();

        if state_after != state_before {
            let zones = alarm.list_zones();
            let _ = self.tx.send(state_after);
            let mut actioneurs = self.actioneurs.lock().unwrap();
            for actioneur in actioneurs.iter_mut() {
                actioneur.on_state_change(state_after, &zones);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggered_ne_normal() {
        assert_ne!(Reading::Triggered, Reading::Normal);
    }

    #[derive(Clone, Default)]
    struct RecordingActioneur {
        calls: Arc<Mutex<Vec<talos_core::State>>>,
    }

    impl Actionneur for RecordingActioneur {
        fn on_state_change(
            &mut self,
            state: talos_core::State,
            _zones: &[(u32, talos_core::ZoneKind, talos_core::ZoneStatus)],
        ) {
            self.calls.lock().unwrap().push(state);
        }
    }

    fn alarm_handle_with(
        alarm: talos_core::Alarm,
        sensor_to_zone: HashMap<String, u32>,
    ) -> (
        AlarmHandle,
        tokio::sync::broadcast::Receiver<talos_core::State>,
        Arc<Mutex<Vec<talos_core::State>>>,
    ) {
        let alarm = Arc::new(Mutex::new(alarm));
        let (tx, rx) = tokio::sync::broadcast::channel(16);
        let recorder = RecordingActioneur::default();
        let calls = recorder.calls.clone();
        let actioneurs: Arc<Mutex<Vec<Box<dyn Actionneur + Send>>>> =
            Arc::new(Mutex::new(vec![Box::new(recorder)]));
        let handle = AlarmHandle::new(alarm, tx, actioneurs, sensor_to_zone);
        (handle, rx, calls)
    }

    #[test]
    fn report_with_unknown_sensor_errors() {
        let (handle, _rx, _calls) = alarm_handle_with(talos_core::Alarm::new(), HashMap::new());

        let result = handle.report("nope", Reading::Triggered);

        assert_eq!(result, Err(ReportError::UnknownSensor("nope".to_string())));
    }

    #[tokio::test]
    async fn report_triggered_for_mapped_sensor_while_armed_changes_zone_and_notifies() {
        let mut alarm = talos_core::Alarm::new();
        alarm.add_zone(1, talos_core::ZoneKind::Instant).unwrap();
        alarm.arm().unwrap();
        alarm.complete_exit_delay().unwrap();
        assert_eq!(alarm.state(), talos_core::State::Armed);

        let mut sensor_to_zone = HashMap::new();
        sensor_to_zone.insert("front-door".to_string(), 1);

        let (handle, mut rx, calls) = alarm_handle_with(alarm, sensor_to_zone);

        handle.report("front-door", Reading::Triggered).unwrap();

        assert_eq!(rx.recv().await.unwrap(), talos_core::State::Triggered);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[talos_core::State::Triggered]
        );
    }
}
