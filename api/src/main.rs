mod auth;
mod db;
mod routes;
mod timers;

use std::sync::{Arc, Mutex};

use axum::{routing::get, Router};
use tower_http::services::ServeDir;

#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::SqlitePool,
    pub jwt_secret: String,
    pub alarm: Arc<Mutex<talos_core::Alarm>>,
    pub tx: tokio::sync::broadcast::Sender<talos_core::State>,
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
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() {
    let jwt_secret = match auth::jwt_secret_from_env() {
        Ok(secret) => secret,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };

    let pool = match db::init_pool(&db::database_url_from_env()).await {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!("failed to initialize database: {err}");
            std::process::exit(1);
        }
    };

    let mut alarm = talos_core::Alarm::new();
    if let Err(err) = db::replay_zones(&pool, &mut alarm).await {
        eprintln!("failed to replay zones: {err}");
        std::process::exit(1);
    }
    let alarm = Arc::new(Mutex::new(alarm));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);

    let exit_delay = timers::exit_delay_from_env();
    let entry_delay = timers::entry_delay_from_env();
    {
        let alarm = Arc::clone(&alarm);
        let tx = tx.clone();
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
                );
            }
        });
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    axum::serve(
        listener,
        app(AppState {
            pool,
            jwt_secret,
            alarm,
            tx,
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
