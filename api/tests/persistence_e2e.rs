//! End-to-end persistence test: spawns the real compiled `api` binary against
//! a real on-disk SQLite database, drives it over the network, restarts it,
//! and confirms both the `users` table and zone state survive the restart
//! (with zone status reset to `Clear` by replay, as designed).
//!
//! `api` has no library target, so this integration test can only interact
//! with the application over the network — there is no way to call its
//! internals directly.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct TestServer {
    child: Child,
}

impl TestServer {
    fn spawn(db_path: &Path, addr: &str) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_api"))
            .env("TALOS_JWT_SECRET", "persistence-e2e-test-secret")
            .env(
                "TALOS_DATABASE_URL",
                format!("sqlite://{}", db_path.display()),
            )
            .env("TALOS_BIND_ADDR", addr)
            .env("TALOS_EXIT_DELAY_SECS", "2")
            .env("TALOS_ENTRY_DELAY_SECS", "2")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn api binary");

        Self { child }
    }

    fn kill_and_wait(&mut self) {
        self.child.kill().expect("failed to kill server process");
        self.child
            .wait()
            .expect("failed to wait for server process to exit");
    }
}

impl Drop for TestServer {
    /// Safety net so a mid-test assertion failure doesn't leak a server
    /// process; the test itself still kills each server explicitly once done.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Sends a hand-written HTTP/1.1 request over a plain `TcpStream` and returns
/// `(status_code, body)`. Sends `Connection: close` so the server closes the
/// socket once the response is complete, letting us just read to EOF.
fn http_request(
    addr: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&Value>,
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;

    let body = body.map(|value| value.to_string()).unwrap_or_default();

    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    if !body.is_empty() {
        request.push_str("Content-Type: application/json\r\n");
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    request.push_str(&body);

    stream.write_all(request.as_bytes())?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    let (headers, resp_body) = response
        .split_once("\r\n\r\n")
        .unwrap_or((response.as_str(), ""));
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);

    Ok((status, resp_body.to_string()))
}

fn wait_until_healthy(addr: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok((status, _)) = http_request(addr, "GET", "/health", None, None) {
            if status == 200 {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "server at {addr} did not become healthy in time"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Binds an ephemeral port to find one that is free, then drops the listener
/// immediately so the address can be reused by the spawned server process.
fn free_local_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind ephemeral port");
    listener
        .local_addr()
        .expect("failed to read local address")
        .to_string()
}

fn extract_token(body: &str) -> String {
    serde_json::from_str::<Value>(body).expect("response was not valid JSON")["token"]
        .as_str()
        .expect("response missing token field")
        .to_string()
}

#[test]
fn zones_and_users_persist_across_restart() {
    let addr = free_local_addr();

    let db_dir = tempfile::tempdir().expect("failed to create temp dir");
    let db_path = db_dir.path().join("persistence_e2e.db");

    let mut server = TestServer::spawn(&db_path, &addr);
    wait_until_healthy(&addr);

    let (status, _) = http_request(
        &addr,
        "POST",
        "/auth/register",
        None,
        Some(&json!({"username": "alice", "password": "hunter2"})),
    )
    .expect("register request failed");
    assert_eq!(status, 201, "expected bootstrap registration to succeed");

    let (status, body) = http_request(
        &addr,
        "POST",
        "/auth/login",
        None,
        Some(&json!({"username": "alice", "password": "hunter2"})),
    )
    .expect("login request failed");
    assert_eq!(status, 200, "expected login to succeed");
    let token = extract_token(&body);

    let (status, _) = http_request(
        &addr,
        "POST",
        "/zones",
        Some(&token),
        Some(&json!({"id": 1, "kind": "Delay"})),
    )
    .expect("create zone request failed");
    assert_eq!(status, 201, "expected zone creation to succeed");

    server.kill_and_wait();

    let mut server = TestServer::spawn(&db_path, &addr);
    wait_until_healthy(&addr);

    let (status, body) = http_request(
        &addr,
        "POST",
        "/auth/login",
        None,
        Some(&json!({"username": "alice", "password": "hunter2"})),
    )
    .expect("fresh login request failed");
    assert_eq!(
        status, 200,
        "expected fresh login after restart to succeed, proving the users table persisted"
    );
    let token = extract_token(&body);

    let (status, body) = http_request(&addr, "GET", "/zones", Some(&token), None)
        .expect("list zones request failed");
    assert_eq!(
        status, 200,
        "expected listing zones after restart to succeed"
    );
    let zones: Value = serde_json::from_str(&body).expect("zones response was not valid JSON");
    assert_eq!(
        zones,
        json!([{"id": 1, "kind": "Delay", "status": "Clear"}]),
        "expected exactly the zone created before the restart, with status reset to Clear by replay"
    );

    server.kill_and_wait();
}
