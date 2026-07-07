mod auth;
mod db;
mod routes;

use axum::{routing::get, Router};

#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::SqlitePool,
    pub jwt_secret: String,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .merge(routes::router())
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

    let pool = match db::init_pool("sqlite://talos.db").await {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!("failed to initialize database: {err}");
            std::process::exit(1);
        }
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    axum::serve(listener, app(AppState { pool, jwt_secret }))
        .await
        .unwrap();
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::{db, AppState};

    pub(crate) async fn state() -> AppState {
        let pool = db::init_pool("sqlite::memory:").await.unwrap();
        AppState {
            pool,
            jwt_secret: "test-secret".to_string(),
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
