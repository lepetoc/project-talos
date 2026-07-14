use crate::Actionneur;

pub struct ShellyModule;

impl Actionneur for ShellyModule {
    fn on_state_change(&mut self, _state: talos_core::State) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_state_change_compiles_and_runs() {
        let mut module = ShellyModule;
        module.on_state_change(talos_core::State::Armed);
    }
}
