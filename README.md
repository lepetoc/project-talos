# Talos

![CI](https://github.com/lepetoc/project-talos/actions/workflows/ci.yml/badge.svg)
![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)

A self-hosted alarm system — state machine, REST/WebSocket API, and a minimal
web interface — built as a personal project, currently running a real
deployment for a coworking space.

## Status: functional core and API, integrations not yet built

The alarm logic itself (arming, disarming, entry/exit delays, zone tracking)
and the API around it (authentication, zone management, real-time state over
WebSocket) are implemented and tested. What is **not** yet in place:

- **No physical sensor integration.** Nothing currently connects this system
  to real hardware (the project uses Shelly devices) — zones today can only
  be triggered by direct API calls, not by an actual door or motion sensor.
- **No connection to a monitoring center.** Reporting events to a real
  central station over [SIA DC-09](https://github.com/lepetoc/sia-rs) is
  planned but not yet wired in.
- **No role-based access control.** Every authenticated account has identical
  permissions; there is no distinction between an administrator and anyone
  else.
- **A known, accepted bootstrap limitation.** The very first account is
  created without authentication, whoever reaches the registration endpoint
  first after a fresh install becomes that first account. This is documented
  in the code (`api/src/routes.rs`) rather than hidden.
- **The web interface is intentionally minimal** — functional, not polished.

If you're evaluating this for your own use, read the code and verify it
against your own requirements — this is a personal project at an early stage,
not a finished product.

## Architecture

- `core/` — the alarm's state machine. No I/O, no external dependencies:
  states, zones, and delay logic only, driven entirely by the caller.
- `api/` — the HTTP/WebSocket server (Axum), SQLite persistence, JWT
  authentication, and the background task that drives `core`'s delays.
- `frontend/` — a single-page interface (Alpine.js via CDN, no build step),
  served directly by `api`.

## Running it

Requires Rust and the environment variables documented in
[`.env.example`](.env.example) — copy it to `.env` and fill in a real value for
`TALOS_JWT_SECRET` at minimum.

```sh
cargo run --package api
```

Precompiled binaries published on GitHub Releases are built with
`--all-features`, so they include every module (`sia_dc09`, `shelly`). That's
a convenience for trying Talos out, not the recommended way to run it —
consistent with the project's principle of not compiling in modules a given
deployment doesn't use, build manually and enable only the features you need,
e.g. `cargo build --package api --no-default-features --features shelly`.

## Contributing

This is primarily a personal project built for a specific deployment, not one
actively seeking contributors — but issues and pull requests are welcome if
something is genuinely broken or worth discussing.

## License

This project is licensed under either of

- [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0)
- [MIT License](https://opensource.org/license/MIT)

at your option.
