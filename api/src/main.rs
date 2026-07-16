mod auth;
mod db;
mod routes;
mod timers;

use std::sync::{Arc, Mutex};

use axum::{routing::get, Router};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::SqlitePool,
    pub jwt_secret: String,
    pub alarm: Arc<Mutex<talos_core::Alarm>>,
    pub tx: tokio::sync::broadcast::Sender<talos_core::State>,
    pub actioneurs: Arc<Mutex<Vec<Box<dyn modules::Actionneur + Send>>>>,
}

/// The frontend lives at the repository root, alongside `core` and `api`, not
/// nested inside this crate — resolved from `CARGO_MANIFEST_DIR` so it works
/// regardless of the process's current working directory.
const FRONTEND_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../frontend");

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .merge(routes::router())
        .fallback_service(ServeDir::new(FRONTEND_DIR))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:3000";

/// Reads the bind address from `TALOS_BIND_ADDR`, the same way the database
/// URL is read in `db.rs`: optional, falling back to a default.
fn bind_addr_from_env() -> String {
    std::env::var("TALOS_BIND_ADDR").unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Loads variables from a `.env` file (searching this directory and its
    // parents) into the process environment, if one exists. Ignored if
    // absent, since in production the variables are typically set directly.
    dotenvy::dotenv().ok();

    let jwt_secret = match auth::jwt_secret_from_env() {
        Ok(secret) => secret,
        Err(err) => {
            error!("{err}");
            std::process::exit(1);
        }
    };

    let pool = match db::init_pool(&db::database_url_from_env()).await {
        Ok(pool) => pool,
        Err(err) => {
            error!("failed to initialize database: {err}");
            std::process::exit(1);
        }
    };

    let mut alarm = talos_core::Alarm::new();
    if let Err(err) = db::replay_zones(&pool, &mut alarm).await {
        error!("failed to replay zones: {err}");
        std::process::exit(1);
    }
    let alarm = Arc::new(Mutex::new(alarm));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);

    #[cfg_attr(not(any(feature = "sia_dc09", feature = "shelly")), allow(unused_mut))]
    let mut actioneurs: Vec<Box<dyn modules::Actionneur + Send>> = Vec::new();
    // Each module's status is one of three states: "disabled" (feature not
    // compiled in), "not configured" (compiled but missing its config), or
    // "active" (actually running).
    #[cfg(feature = "sia_dc09")]
    let sia_dc09_status;
    #[cfg(not(feature = "sia_dc09"))]
    let sia_dc09_status = "disabled";
    #[cfg(feature = "sia_dc09")]
    {
        match db::get_sia_config(&pool).await {
            Ok(Some((account, prefix, receiver_addr))) => {
                match modules::sia_dc09::SiaDc09Module::new(&account, &prefix, &receiver_addr) {
                    Ok(module) => {
                        actioneurs.push(Box::new(module));
                        sia_dc09_status = "active";
                    }
                    Err(err) => {
                        error!("failed to initialize sia_dc09 module: {err}");
                        std::process::exit(1);
                    }
                }
            }
            Ok(None) => {
                info!("SIA DC-09 is not yet configured; skipping module initialization");
                sia_dc09_status = "not configured";
            }
            Err(err) => {
                error!("failed to load sia_dc09 config: {err}");
                std::process::exit(1);
            }
        }
    }
    #[cfg(feature = "shelly")]
    let shelly_status = "active";
    #[cfg(not(feature = "shelly"))]
    let shelly_status = "disabled";
    #[cfg(feature = "shelly")]
    {
        actioneurs.push(Box::new(modules::shelly::ShellyModule));
    }
    let actioneurs = Arc::new(Mutex::new(actioneurs));

    // The shelly diagnostic listener runs whenever the feature is compiled
    // in, but only attempts a connection once a gateway address is set.
    #[cfg(feature = "shelly")]
    let shelly_gateway = match db::get_shelly_gateway_addr(&pool).await {
        Ok(Some(_)) => "configured",
        Ok(None) => "not configured",
        Err(err) => {
            tracing::warn!("failed to read shelly gateway address: {err}");
            "unknown"
        }
    };
    #[cfg(not(feature = "shelly"))]
    let shelly_gateway = "disabled";
    info!(
        sia_dc09 = sia_dc09_status,
        shelly = shelly_status,
        shelly_gateway = shelly_gateway,
        "modules initialized"
    );

    #[cfg(feature = "shelly")]
    {
        let sensor_to_zone = match db::load_sensor_mappings(&pool).await {
            Ok(map) => map,
            Err(err) => {
                error!("failed to load sensor mappings: {err}");
                std::process::exit(1);
            }
        };
        // Constructed at startup so the sensor-to-zone map is loaded once;
        // not yet wired to an HTTP endpoint that would call `report` — that
        // arrives with the actual Shelly webhook handler.
        let _alarm_handle = modules::AlarmHandle::new(
            Arc::clone(&alarm),
            tx.clone(),
            Arc::clone(&actioneurs),
            sensor_to_zone,
        );
    }

    let exit_delay = timers::exit_delay_from_env();
    let entry_delay = timers::entry_delay_from_env();
    {
        let alarm = Arc::clone(&alarm);
        let tx = tx.clone();
        let actioneurs = Arc::clone(&actioneurs);
        tokio::spawn(async move {
            let mut tracker = timers::StateTracker::new();
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                timers::check(
                    &alarm,
                    &mut tracker,
                    exit_delay,
                    entry_delay,
                    std::time::Instant::now(),
                    &tx,
                    &actioneurs,
                );
            }
        });
    }

    // Diagnostic-phase listener: re-reads the gateway address on every
    // iteration, so a dropped connection or a reconfigured address are both
    // handled by simply looping back around.
    #[cfg(feature = "shelly")]
    {
        let pool = pool.clone();
        tokio::spawn(async move {
            loop {
                match db::get_shelly_gateway_addr(&pool).await {
                    Ok(Some(addr)) => {
                        if let Err(err) = modules::shelly::run_diagnostic_listener(&addr).await {
                            tracing::warn!("shelly diagnostic listener failed: {err}");
                        }
                    }
                    // No gateway configured yet: an ordinary state, not worth logging.
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!("failed to read shelly gateway address: {err}");
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(bind_addr_from_env())
        .await
        .unwrap();
    info!(addr = %listener.local_addr().unwrap(), "listening");
    axum::serve(
        listener,
        app(AppState {
            pool,
            jwt_secret,
            alarm,
            tx,
            actioneurs,
        }),
    )
    .await
    .unwrap();
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use crate::{db, AppState};

    pub(crate) async fn state() -> AppState {
        let pool = db::init_pool("sqlite::memory:").await.unwrap();
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        AppState {
            pool,
            jwt_secret: "test-secret".to_string(),
            alarm: Arc::new(Mutex::new(talos_core::Alarm::new())),
            tx,
            actioneurs: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_returns_ok() {
        let response = app(test_support::state().await)
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"ok");
    }
}
