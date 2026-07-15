use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use crate::Actionneur;

pub struct ShellyModule;

/// Connects to the Shelly gateway's RPC websocket and logs every incoming
/// frame verbatim, for observing the real device's notification format
/// before any parsing is written. The initial `Shelly.GetDeviceInfo` call is
/// a best guess at what makes the gateway start pushing notifications; it
/// will be adjusted once tested against the real hardware.
pub async fn run_diagnostic_listener(gateway_addr: &str) -> Result<(), String> {
    let url = format!("ws://{gateway_addr}/rpc");
    let (mut ws, _response) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|err| format!("failed to connect to {url}: {err}"))?;

    ws.send(Message::text(
        r#"{"id": 1, "src": "talos", "method": "Shelly.GetDeviceInfo"}"#,
    ))
    .await
    .map_err(|err| format!("failed to send initial request to {url}: {err}"))?;

    while let Some(message) = ws.next().await {
        match message {
            Ok(message) => {
                tracing::info!(target: "shelly_diagnostic", raw = %message, "received frame");
            }
            Err(err) => return Err(format!("connection to {url} failed: {err}")),
        }
    }

    Ok(())
}

impl Actionneur for ShellyModule {
    fn on_state_change(
        &mut self,
        _state: talos_core::State,
        _zones: &[(u32, talos_core::ZoneKind, talos_core::ZoneStatus)],
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_state_change_compiles_and_runs() {
        let mut module = ShellyModule;
        module.on_state_change(talos_core::State::Armed, &[]);
    }
}
