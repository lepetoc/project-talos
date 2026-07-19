use axum::http::{header, HeaderValue, Method};
use tower_http::cors::{AllowOrigin, CorsLayer};

const ALLOWED_METHODS: [Method; 4] = [Method::GET, Method::POST, Method::PUT, Method::DELETE];

/// Reads `TALOS_ALLOWED_ORIGINS`, a comma-separated list of allowed origins
/// (e.g. "https://talos.example.com,http://10.1.11.50:3000"), and builds a
/// `CorsLayer` restricted to exactly those origins. Returns `Ok(None)` if the
/// variable is unset or empty, leaving cross-origin requests blocked by the
/// browser's default same-origin policy — the safe default.
pub fn cors_layer_from_env() -> Result<Option<CorsLayer>, String> {
    match std::env::var("TALOS_ALLOWED_ORIGINS") {
        Ok(value) => cors_layer_for(&value),
        Err(_) => Ok(None),
    }
}

/// The parsing logic behind [`cors_layer_from_env`], split out so it can be
/// exercised directly (unit tests, and the router-level CORS tests in
/// `main.rs`) without mutating process-wide environment state.
pub(crate) fn cors_layer_for(value: &str) -> Result<Option<CorsLayer>, String> {
    let origins = value
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(|origin| {
            origin
                .parse::<HeaderValue>()
                .map_err(|_| format!("invalid origin in TALOS_ALLOWED_ORIGINS: {origin}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    if origins.is_empty() {
        return Ok(None);
    }

    Ok(Some(
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods(ALLOWED_METHODS.to_vec())
            .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_empty_value_yields_no_layer() {
        assert!(cors_layer_for("").unwrap().is_none());
        assert!(cors_layer_for(" , ,").unwrap().is_none());
    }

    #[test]
    fn configured_origins_yield_a_layer() {
        assert!(
            cors_layer_for("https://talos.example.com, http://10.1.11.50:3000")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn invalid_origin_is_rejected() {
        assert!(cors_layer_for("https://talos.example.com\nhttps://evil.example.com").is_err());
    }
}
