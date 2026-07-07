use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{self, AuthUser};
use crate::db;
use crate::AppState;

const INVALID_CREDENTIALS: &str = "invalid username or password";

#[derive(Deserialize)]
pub struct RegisterRequest {
    username: String,
    password: String,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
pub struct TokenResponse {
    token: String,
}

enum ApiError {
    Unauthorized(&'static str),
    Conflict,
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::Unauthorized(message) => {
                (StatusCode::UNAUTHORIZED, Json(json!({ "error": message }))).into_response()
            }
            ApiError::Conflict => StatusCode::CONFLICT.into_response(),
            ApiError::Internal => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
}

/// Creates a user account.
///
/// Known, accepted limitation: while the `users` table is empty, this endpoint
/// creates the account with no authentication required at all — anyone able to
/// reach it before a first account exists can create the initial account. This is
/// intentional for the current phase, not an oversight.
async fn register(
    State(state): State<AppState>,
    auth: Option<AuthUser>,
    Json(payload): Json<RegisterRequest>,
) -> Result<StatusCode, ApiError> {
    let user_count = db::count_users(&state.pool)
        .await
        .map_err(|_| ApiError::Internal)?;

    if user_count > 0 && auth.is_none() {
        return Err(ApiError::Unauthorized("authentication required"));
    }

    let password_hash = auth::hash_password(&payload.password).map_err(|_| ApiError::Internal)?;

    match db::insert_user(&state.pool, &payload.username, &password_hash).await {
        Ok(_) => Ok(StatusCode::CREATED),
        Err(db::InsertUserError::UsernameTaken) => Err(ApiError::Conflict),
        Err(db::InsertUserError::Other(_)) => Err(ApiError::Internal),
    }
}

async fn login(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    let user = db::find_user_by_username(&state.pool, &payload.username)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or(ApiError::Unauthorized(INVALID_CREDENTIALS))?;

    let valid = auth::verify_password(&payload.password, &user.password_hash)
        .map_err(|_| ApiError::Internal)?;
    if !valid {
        return Err(ApiError::Unauthorized(INVALID_CREDENTIALS));
    }

    let token = auth::create_token(user.id, &state.jwt_secret).map_err(|_| ApiError::Internal)?;
    Ok(Json(TokenResponse { token }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app, test_support};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn json_request(uri: &str, body: serde_json::Value, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn register_first_user_without_token_succeeds() {
        let router = app(test_support::state().await);

        let response = router
            .oneshot(json_request(
                "/auth/register",
                json!({ "username": "alice", "password": "hunter2" }),
                None,
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn register_second_user_without_token_fails() {
        let router = app(test_support::state().await);

        router
            .clone()
            .oneshot(json_request(
                "/auth/register",
                json!({ "username": "alice", "password": "hunter2" }),
                None,
            ))
            .await
            .unwrap();

        let response = router
            .oneshot(json_request(
                "/auth/register",
                json!({ "username": "bob", "password": "hunter3" }),
                None,
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn register_second_user_with_valid_token_succeeds() {
        let router = app(test_support::state().await);

        router
            .clone()
            .oneshot(json_request(
                "/auth/register",
                json!({ "username": "alice", "password": "hunter2" }),
                None,
            ))
            .await
            .unwrap();

        let login_response = router
            .clone()
            .oneshot(json_request(
                "/auth/login",
                json!({ "username": "alice", "password": "hunter2" }),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(login_response.status(), StatusCode::OK);
        let token = body_json(login_response).await["token"]
            .as_str()
            .unwrap()
            .to_string();

        let response = router
            .oneshot(json_request(
                "/auth/register",
                json!({ "username": "bob", "password": "hunter3" }),
                Some(&token),
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn registering_an_existing_username_conflicts() {
        let router = app(test_support::state().await);

        router
            .clone()
            .oneshot(json_request(
                "/auth/register",
                json!({ "username": "alice", "password": "hunter2" }),
                None,
            ))
            .await
            .unwrap();

        let login_response = router
            .clone()
            .oneshot(json_request(
                "/auth/login",
                json!({ "username": "alice", "password": "hunter2" }),
                None,
            ))
            .await
            .unwrap();
        let token = body_json(login_response).await["token"]
            .as_str()
            .unwrap()
            .to_string();

        let response = router
            .oneshot(json_request(
                "/auth/register",
                json!({ "username": "alice", "password": "different" }),
                Some(&token),
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn login_with_correct_password_returns_token() {
        let router = app(test_support::state().await);

        router
            .clone()
            .oneshot(json_request(
                "/auth/register",
                json!({ "username": "alice", "password": "hunter2" }),
                None,
            ))
            .await
            .unwrap();

        let response = router
            .oneshot(json_request(
                "/auth/login",
                json!({ "username": "alice", "password": "hunter2" }),
                None,
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert!(body["token"].as_str().is_some());
    }

    #[tokio::test]
    async fn login_with_wrong_password_and_unknown_username_return_identical_401() {
        let router = app(test_support::state().await);

        router
            .clone()
            .oneshot(json_request(
                "/auth/register",
                json!({ "username": "alice", "password": "hunter2" }),
                None,
            ))
            .await
            .unwrap();

        let wrong_password_response = router
            .clone()
            .oneshot(json_request(
                "/auth/login",
                json!({ "username": "alice", "password": "wrong" }),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(wrong_password_response.status(), StatusCode::UNAUTHORIZED);
        let wrong_password_body = body_json(wrong_password_response).await;

        let unknown_username_response = router
            .oneshot(json_request(
                "/auth/login",
                json!({ "username": "nobody", "password": "whatever" }),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(unknown_username_response.status(), StatusCode::UNAUTHORIZED);
        let unknown_username_body = body_json(unknown_username_response).await;

        assert_eq!(wrong_password_body, unknown_username_body);
    }
}
