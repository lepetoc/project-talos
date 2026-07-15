use std::fmt;
use std::io::{Read, Write};
use std::net::TcpStream;

use tracing::{info, warn};

use crate::Actionneur;

const ACCOUNT_ENV: &str = "TALOS_SIA_ACCOUNT";
const PREFIX_ENV: &str = "TALOS_SIA_PREFIX";
const RECEIVER_ADDR_ENV: &str = "TALOS_SIA_RECEIVER_ADDR";
const DEFAULT_PREFIX: &str = "0";

#[derive(Debug)]
pub enum ConfigError {
    MissingEnv(&'static str),
    InvalidAccount(sia_rs::AccountError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::MissingEnv(key) => write!(f, "{key} must be set"),
            ConfigError::InvalidAccount(err) => write!(f, "invalid {ACCOUNT_ENV}: {err}"),
        }
    }
}

impl std::error::Error for ConfigError {}

pub struct SiaDc09Module {
    client: sia_rs::Client,
    receiver_addr: String,
}

impl SiaDc09Module {
    pub fn from_env() -> Result<Self, ConfigError> {
        let account_number =
            std::env::var(ACCOUNT_ENV).map_err(|_| ConfigError::MissingEnv(ACCOUNT_ENV))?;
        let prefix = std::env::var(PREFIX_ENV).unwrap_or_else(|_| DEFAULT_PREFIX.to_string());
        let receiver_addr = std::env::var(RECEIVER_ADDR_ENV)
            .map_err(|_| ConfigError::MissingEnv(RECEIVER_ADDR_ENV))?;

        let account = sia_rs::Account::new(&account_number, &prefix, None)
            .map_err(ConfigError::InvalidAccount)?;

        Ok(SiaDc09Module {
            client: sia_rs::Client::new(account),
            receiver_addr,
        })
    }
}

impl Actionneur for SiaDc09Module {
    fn on_state_change(
        &mut self,
        state: talos_core::State,
        zones: &[(u32, talos_core::ZoneKind, talos_core::ZoneStatus)],
    ) {
        if state != talos_core::State::Triggered {
            return;
        }

        let zone_id = zones
            .iter()
            .find(|(_, _, status)| *status == talos_core::ZoneStatus::Triggered)
            .map(|(id, _, _)| *id);
        let Some(zone_id) = zone_id else {
            return;
        };

        let code = format!("NBA{zone_id:04}");
        let message = self.client.build_event(&code);
        let receiver_addr = self.receiver_addr.clone();

        std::thread::spawn(move || {
            let mut stream = match TcpStream::connect(&receiver_addr) {
                Ok(stream) => stream,
                Err(err) => {
                    warn!(%err, %receiver_addr, "failed to connect to SIA receiver");
                    return;
                }
            };

            if let Err(err) = stream.write_all(&message) {
                warn!(%err, %receiver_addr, "failed to send SIA event to receiver");
                return;
            }

            let mut buffer = [0u8; 128];
            let bytes_read = match stream.read(&mut buffer) {
                Ok(n) => n,
                Err(err) => {
                    warn!(%err, %receiver_addr, "failed to read SIA receiver response");
                    return;
                }
            };

            match sia_rs::check_response(&buffer[..bytes_read]) {
                Ok(()) => info!(%receiver_addr, "SIA event acknowledged by receiver"),
                Err(err) => warn!(%err, %receiver_addr, "SIA receiver rejected event"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[test]
    fn on_state_change_sends_event_naming_the_triggered_zone() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        // SAFETY: this test runs single-threaded within this process (no other
        // test in this crate reads or writes these variables), so there is no
        // concurrent access to race against.
        unsafe {
            std::env::set_var(ACCOUNT_ENV, "1234");
            std::env::remove_var(PREFIX_ENV);
            std::env::set_var(RECEIVER_ADDR_ENV, addr.to_string());
        }
        let mut module = SiaDc09Module::from_env().unwrap();

        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 256];
            let bytes_read = socket.read(&mut buffer).unwrap();
            done_tx.send(buffer[..bytes_read].to_vec()).unwrap();
            // Unblocks the module's spawned thread, which is waiting on its own
            // read() for a response; the exact contents don't matter to this test.
            let _ = socket.write_all(b"\x0A0000000B\"ACK\"\x0D");
        });

        let zones = [(
            7,
            talos_core::ZoneKind::Instant,
            talos_core::ZoneStatus::Triggered,
        )];
        module.on_state_change(talos_core::State::Triggered, &zones);

        let received = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let received = String::from_utf8_lossy(&received);
        assert!(received.contains("NBA0007"), "got: {received}");
    }
}
