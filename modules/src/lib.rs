#[cfg(feature = "shelly")]
pub mod shelly;
#[cfg(feature = "sia_dc09")]
pub mod sia_dc09;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggered_ne_normal() {
        assert_ne!(Reading::Triggered, Reading::Normal);
    }
}
