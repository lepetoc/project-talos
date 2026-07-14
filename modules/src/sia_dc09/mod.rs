use crate::Actionneur;

pub struct SiaDc09Module;

impl Actionneur for SiaDc09Module {
    fn on_state_change(&mut self, _state: talos_core::State) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_state_change_compiles_and_runs() {
        let mut module = SiaDc09Module;
        module.on_state_change(talos_core::State::Armed);
    }
}
