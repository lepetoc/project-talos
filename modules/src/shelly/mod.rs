use base64::Engine;
use btsensor::bthome::v2::{BtHomeV2, Element};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::{Actionneur, AlarmHandle, Reading};

pub struct ShellyModule;

/// The BTHome v2 service UUID's 16-bit alias (0xFCD2), in the little-endian
/// order it appears on the wire in a Service Data AD structure. Bytes 2..4 of
/// the 128-bit constant hold the alias, big-endian.
const BTHOME_UUID_LE: [u8; 2] = [
    btsensor::bthome::v2::UUID.as_bytes()[3],
    btsensor::bthome::v2::UUID.as_bytes()[2],
];

/// Connects to the Shelly gateway's RPC websocket and reports BTHome sensor
/// readings from `ble.scan_result` notifications to the alarm. The initial
/// `Shelly.GetDeviceInfo` call makes the gateway start pushing notifications
/// to this client.
pub async fn run_listener(gateway_addr: &str, alarm: &AlarmHandle) -> Result<(), String> {
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
            Ok(Message::Text(text)) => handle_frame(&text, alarm),
            Ok(_) => {}
            Err(err) => return Err(format!("connection to {url} failed: {err}")),
        }
    }

    Ok(())
}

fn handle_frame(text: &str, alarm: &AlarmHandle) {
    let Ok(frame) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    let Some(events) = frame["params"]["events"].as_array() else {
        return;
    };
    for event in events {
        if event["event"] != "ble.scan_result" {
            continue;
        }
        // `data` is `[count, [[mac, rssi, payload, name], ...]]` — the entry
        // list sits at index 1 behind a batch count.
        let Some(entries) = event["data"].get(1).and_then(|v| v.as_array()) else {
            warn!(data = %event["data"], "unexpected ble.scan_result data shape");
            return;
        };
        for entry in entries {
            process_scan_entry(entry, alarm);
        }
    }
}

fn process_scan_entry(entry: &serde_json::Value, alarm: &AlarmHandle) {
    let (Some(mac), Some(payload)) = (entry[0].as_str(), entry[2].as_str()) else {
        warn!(%entry, "malformed ble.scan_result entry");
        return;
    };
    // The scan stream includes every nearby Bluetooth device, so this stays
    // below default log visibility.
    debug!(mac, rssi = %entry[1], name = %entry[3], "ble scan result");

    let Some(sensor_id) = alarm.canonical_sensor_id(mac) else {
        return;
    };

    match decode_reading(payload) {
        Ok(reading) => match alarm.report(&sensor_id, reading) {
            Ok(()) => info!(
                sensor = sensor_id.as_str(),
                ?reading,
                "sensor reading reported"
            ),
            Err(err) => {
                warn!(
                    sensor = sensor_id.as_str(),
                    "failed to report sensor reading: {err}"
                )
            }
        },
        Err(err) => warn!(
            sensor = sensor_id.as_str(),
            payload, "failed to decode sensor payload: {err}"
        ),
    }
}

/// Decodes a base64 BLE advertisement payload into an alarm reading: finds
/// the BTHome v2 service data among the payload's AD structures, decodes it,
/// and maps its opening (0x11) or motion (0x21) binary sensor — whichever the
/// sensor reports — to a reading.
fn decode_reading(payload_b64: &str) -> Result<Reading, String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(payload_b64)
        .map_err(|err| format!("invalid base64: {err}"))?;
    let service_data = bthome_service_data(&raw).ok_or("no BTHome v2 service data in payload")?;
    let decoded =
        BtHomeV2::decode(service_data).map_err(|err| format!("BTHome decode failed: {err}"))?;
    let triggered = decoded
        .elements
        .iter()
        .find_map(|element| match element {
            Element::Open(open) => Some(*open),
            Element::MotionDetected(motion) => Some(*motion),
            _ => None,
        })
        .ok_or_else(|| format!("no opening or motion element in: {decoded}"))?;
    Ok(if triggered {
        Reading::Triggered
    } else {
        Reading::Normal
    })
}

/// Finds the BTHome v2 service data in a raw BLE advertisement payload. The
/// payload is a sequence of AD structures — one length byte, one type byte,
/// then `length - 1` data bytes — and the BTHome payload is the data of the
/// Service Data - 16-bit UUID structure (type 0x16) whose leading UUID is
/// BTHome's.
fn bthome_service_data(raw: &[u8]) -> Option<&[u8]> {
    let mut rest = raw;
    while let [len, tail @ ..] = rest {
        let len = *len as usize;
        if len == 0 || tail.len() < len {
            return None;
        }
        let (structure, tail) = tail.split_at(len);
        if let [0x16, uuid_lo, uuid_hi, payload @ ..] = structure {
            if [*uuid_lo, *uuid_hi] == BTHOME_UUID_LE {
                return Some(payload);
            }
        }
        rest = tail;
    }
    None
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

    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn on_state_change_compiles_and_runs() {
        let mut module = ShellyModule;
        module.on_state_change(talos_core::State::Armed, &[]);
    }

    // Real payloads captured from the actual SBMO-003Z motion sensor during
    // hardware testing, manually decoded and verified byte-by-byte:
    // device info 0x44 (BTHome v2, unencrypted), packet id, battery,
    // illuminance, then object id 0x21 (motion) with value 0x00 or 0x01.

    #[test]
    fn decode_reading_real_capture_motion_normal() {
        // Captured 2026-07-16, motion object (0x21) = 0x00.
        assert_eq!(
            decode_reading("AgEGDhbS/EQApAFkBYSZACEA").unwrap(),
            Reading::Normal
        );
    }

    #[test]
    fn decode_reading_real_capture_motion_triggered() {
        // Captured 2026-07-16, motion object (0x21) = 0x01.
        assert_eq!(
            decode_reading("AgEGDhbS/EQApQFkBYSyACEB").unwrap(),
            Reading::Triggered
        );
    }

    #[test]
    fn decode_reading_rejects_invalid_base64() {
        assert!(decode_reading("not valid base64!!!").is_err());
    }

    #[test]
    fn decode_reading_rejects_payload_without_bthome_service_data() {
        // Valid base64, valid AD structure, but no Service Data - 16-bit UUID
        // structure carrying the BTHome UUID at all — e.g. just flags.
        // Encode "0201060303AABB" (flags + an unrelated 16-bit service UUID
        // list) as the payload.
        let payload = base64::engine::general_purpose::STANDARD.encode(hex_bytes("0201060303aabb"));
        assert!(decode_reading(&payload).is_err());
    }

    #[test]
    fn decode_reading_rejects_bthome_payload_without_opening_or_motion() {
        // A real BTHome structure containing only packet id, battery, and
        // illuminance — no opening (0x11) or motion (0x21) object — should
        // fail with a clear "no opening or motion element" error, not decode
        // successfully with some default. This is the real captured payload
        // with its trailing motion object (`2100`) removed and the AD length
        // byte recomputed accordingly (0x0c, not the original 0x0e).
        let payload = base64::engine::general_purpose::STANDARD
            .encode(hex_bytes("0201060c16d2fc4400a4016405849900"));
        let err = decode_reading(&payload).unwrap_err();
        assert!(err.contains("no opening or motion"));
    }

    #[test]
    fn bthome_service_data_finds_payload_after_uuid() {
        let raw = hex_bytes("0201060e16d2fc4400a40164058499002100");
        let data = bthome_service_data(&raw).unwrap();
        assert_eq!(data, hex_bytes("4400a40164058499002100").as_slice());
    }

    #[test]
    fn bthome_service_data_returns_none_without_bthome_uuid() {
        let raw = hex_bytes("0201060303aabb");
        assert!(bthome_service_data(&raw).is_none());
    }

    #[test]
    fn bthome_service_data_returns_none_on_truncated_structure() {
        // A length byte claiming more data than actually follows must not
        // panic — just report no match.
        let raw = hex_bytes("ff16d2fc44");
        assert!(bthome_service_data(&raw).is_none());
    }

    #[test]
    fn bthome_service_data_returns_none_on_empty_input() {
        assert!(bthome_service_data(&[]).is_none());
    }
}
