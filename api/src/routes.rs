use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
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

#[derive(Deserialize)]
pub struct CreateZoneRequest {
    id: u32,
    kind: String,
}

#[derive(Serialize)]
pub struct ZoneResponse {
    id: u32,
    kind: String,
    status: String,
}

#[derive(Serialize)]
pub struct StateResponse {
    state: String,
}

fn state_to_str(state: talos_core::State) -> &'static str {
    match state {
        talos_core::State::Disarmed => "Disarmed",
        talos_core::State::ExitDelay => "ExitDelay",
        talos_core::State::Armed => "Armed",
        talos_core::State::EntryDelay => "EntryDelay",
        talos_core::State::Triggered => "Triggered",
    }
}

enum ApiError {
    BadRequest(&'static str),
    Unauthorized(&'static str),
    NotFound,
    Conflict,
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::BadRequest(message) => {
                (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
            }
            ApiError::Unauthorized(message) => {
                (StatusCode::UNAUTHORIZED, Json(json!({ "error": message }))).into_response()
            }
            ApiError::NotFound => StatusCode::NOT_FOUND.into_response(),
            ApiError::Conflict => StatusCode::CONFLICT.into_response(),
            ApiError::Internal => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/zones", post(create_zone).get(list_zones))
        .route("/zones/{id}", delete(delete_zone))
        .route("/arm", post(arm))
        .route("/disarm", post(disarm))
        .route("/state", get(get_state))
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

async fn create_zone(
    State(state): State<AppState>,
    _auth: AuthUser,
    Json(payload): Json<CreateZoneRequest>,
) -> Result<StatusCode, ApiError> {
    let kind = db::parse_zone_kind(&payload.kind)
        .map_err(|_| ApiError::BadRequest("invalid zone kind"))?;

    {
        let mut alarm = state.alarm.lock().unwrap();
        if alarm.add_zone(payload.id, kind).is_err() {
            return Err(ApiError::Conflict);
        }
    }

    if db::insert_zone(&state.pool, payload.id as i64, kind)
        .await
        .is_err()
    {
        state
            .alarm
            .lock()
            .unwrap()
            .remove_zone(payload.id)
            .expect("a zone just added is always Clear and removable");
        return Err(ApiError::Internal);
    }

    Ok(StatusCode::CREATED)
}

async fn list_zones(State(state): State<AppState>, _auth: AuthUser) -> Json<Vec<ZoneResponse>> {
    let zones = state.alarm.lock().unwrap().list_zones();
    Json(
        zones
            .into_iter()
            .map(|(id, kind, status)| ZoneResponse {
                id,
                kind: db::zone_kind_to_str(kind).to_string(),
                status: db::zone_status_to_str(status).to_string(),
            })
            .collect(),
    )
}

async fn delete_zone(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<u32>,
) -> Result<StatusCode, ApiError> {
    let kind = {
        let mut alarm = state.alarm.lock().unwrap();
        let kind = alarm
            .list_zones()
            .into_iter()
            .find(|(zone_id, _, _)| *zone_id == id)
            .map(|(_, kind, _)| kind);

        match alarm.remove_zone(id) {
            Ok(()) => {}
            Err(talos_core::RemoveZoneError::UnknownZone(_)) => return Err(ApiError::NotFound),
            Err(talos_core::RemoveZoneError::ZoneTriggered(_)) => return Err(ApiError::Conflict),
        }

        kind.expect("zone existed a moment ago since remove_zone just succeeded")
    };

    if db::delete_zone(&state.pool, id as i64).await.is_err() {
        state
            .alarm
            .lock()
            .unwrap()
            .add_zone(id, kind)
            .expect("zone should not already exist right after its own removal");
        return Err(ApiError::Internal);
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn arm(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<StateResponse>, ApiError> {
    let mut alarm = state.alarm.lock().unwrap();
    alarm.arm().map_err(|_| ApiError::Conflict)?;
    Ok(Json(StateResponse {
        state: state_to_str(alarm.state()).to_string(),
    }))
}

async fn disarm(State(state): State<AppState>, _auth: AuthUser) -> Json<StateResponse> {
    let mut alarm = state.alarm.lock().unwrap();
    alarm.disarm();
    Json(StateResponse {
        state: state_to_str(alarm.state()).to_string(),
    })
}

async fn get_state(State(state): State<AppState>, _auth: AuthUser) -> Json<StateResponse> {
    let alarm = state.alarm.lock().unwrap();
    Json(StateResponse {
        state: state_to_str(alarm.state()).to_string(),
    })
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

    fn get_request(uri: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method("GET").uri(uri);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    fn delete_request(uri: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method("DELETE").uri(uri);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn register_and_login(router: &Router, username: &str, password: &str) -> String {
        router
            .clone()
            .oneshot(json_request(
                "/auth/register",
                json!({ "username": username, "password": password }),
                None,
            ))
            .await
            .unwrap();

        let login_response = router
            .clone()
            .oneshot(json_request(
                "/auth/login",
                json!({ "username": username, "password": password }),
                None,
            ))
            .await
            .unwrap();

        body_json(login_response).await["token"]
            .as_str()
            .unwrap()
            .to_string()
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

    #[tokio::test]
    async fn create_zone_without_token_fails() {
        let router = app(test_support::state().await);

        let response = router
            .oneshot(json_request(
                "/zones",
                json!({ "id": 1, "kind": "Delay" }),
                None,
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_zone_with_token_succeeds_and_appears_in_list() {
        let router = app(test_support::state().await);
        let token = register_and_login(&router, "alice", "hunter2").await;

        let create_response = router
            .clone()
            .oneshot(json_request(
                "/zones",
                json!({ "id": 1, "kind": "Delay" }),
                Some(&token),
            ))
            .await
            .unwrap();
        assert_eq!(create_response.status(), StatusCode::CREATED);

        let list_response = router
            .oneshot(get_request("/zones", Some(&token)))
            .await
            .unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let zones = body_json(list_response).await;
        assert_eq!(
            zones,
            json!([{ "id": 1, "kind": "Delay", "status": "Clear" }])
        );
    }

    #[tokio::test]
    async fn create_zone_with_invalid_kind_bad_request() {
        let router = app(test_support::state().await);
        let token = register_and_login(&router, "alice", "hunter2").await;

        let response = router
            .oneshot(json_request(
                "/zones",
                json!({ "id": 1, "kind": "NotAKind" }),
                Some(&token),
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_zone_with_duplicate_id_conflicts() {
        let router = app(test_support::state().await);
        let token = register_and_login(&router, "alice", "hunter2").await;

        router
            .clone()
            .oneshot(json_request(
                "/zones",
                json!({ "id": 1, "kind": "Delay" }),
                Some(&token),
            ))
            .await
            .unwrap();

        let response = router
            .oneshot(json_request(
                "/zones",
                json!({ "id": 1, "kind": "Instant" }),
                Some(&token),
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn list_zones_reflects_current_status() {
        let state = test_support::state().await;
        let router = app(state.clone());
        let token = register_and_login(&router, "alice", "hunter2").await;

        router
            .clone()
            .oneshot(json_request(
                "/zones",
                json!({ "id": 1, "kind": "Instant" }),
                Some(&token),
            ))
            .await
            .unwrap();

        state
            .alarm
            .lock()
            .unwrap()
            .report_zone_event(1, talos_core::ZoneStatus::Triggered)
            .unwrap();

        let response = router
            .oneshot(get_request("/zones", Some(&token)))
            .await
            .unwrap();
        let zones = body_json(response).await;
        assert_eq!(
            zones,
            json!([{ "id": 1, "kind": "Instant", "status": "Triggered" }])
        );
    }

    #[tokio::test]
    async fn delete_clear_zone_succeeds_and_disappears() {
        let router = app(test_support::state().await);
        let token = register_and_login(&router, "alice", "hunter2").await;

        router
            .clone()
            .oneshot(json_request(
                "/zones",
                json!({ "id": 1, "kind": "Delay" }),
                Some(&token),
            ))
            .await
            .unwrap();

        let delete_response = router
            .clone()
            .oneshot(delete_request("/zones/1", Some(&token)))
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

        let list_response = router
            .oneshot(get_request("/zones", Some(&token)))
            .await
            .unwrap();
        let zones = body_json(list_response).await;
        assert_eq!(zones, json!([]));
    }

    #[tokio::test]
    async fn delete_triggered_zone_conflicts_and_remains() {
        let state = test_support::state().await;
        let router = app(state.clone());
        let token = register_and_login(&router, "alice", "hunter2").await;

        router
            .clone()
            .oneshot(json_request(
                "/zones",
                json!({ "id": 1, "kind": "Instant" }),
                Some(&token),
            ))
            .await
            .unwrap();

        state
            .alarm
            .lock()
            .unwrap()
            .report_zone_event(1, talos_core::ZoneStatus::Triggered)
            .unwrap();

        let delete_response = router
            .clone()
            .oneshot(delete_request("/zones/1", Some(&token)))
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::CONFLICT);

        let list_response = router
            .oneshot(get_request("/zones", Some(&token)))
            .await
            .unwrap();
        let zones = body_json(list_response).await;
        assert_eq!(
            zones,
            json!([{ "id": 1, "kind": "Instant", "status": "Triggered" }])
        );
    }

    #[tokio::test]
    async fn delete_unknown_zone_not_found() {
        let router = app(test_support::state().await);
        let token = register_and_login(&router, "alice", "hunter2").await;

        let response = router
            .oneshot(delete_request("/zones/42", Some(&token)))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_zone_without_token_fails() {
        let router = app(test_support::state().await);

        let response = router
            .oneshot(delete_request("/zones/1", None))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    fn post_request(uri: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method("POST").uri(uri);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn arm_succeeds_and_state_reports_exit_delay() {
        let router = app(test_support::state().await);
        let token = register_and_login(&router, "alice", "hunter2").await;

        let arm_response = router
            .clone()
            .oneshot(post_request("/arm", Some(&token)))
            .await
            .unwrap();
        assert_eq!(arm_response.status(), StatusCode::OK);
        assert_eq!(
            body_json(arm_response).await,
            json!({ "state": "ExitDelay" })
        );

        let state_response = router
            .oneshot(get_request("/state", Some(&token)))
            .await
            .unwrap();
        assert_eq!(state_response.status(), StatusCode::OK);
        assert_eq!(
            body_json(state_response).await,
            json!({ "state": "ExitDelay" })
        );
    }

    #[tokio::test]
    async fn arming_twice_conflicts_and_state_unchanged() {
        let router = app(test_support::state().await);
        let token = register_and_login(&router, "alice", "hunter2").await;

        router
            .clone()
            .oneshot(post_request("/arm", Some(&token)))
            .await
            .unwrap();

        let second_arm_response = router
            .clone()
            .oneshot(post_request("/arm", Some(&token)))
            .await
            .unwrap();
        assert_eq!(second_arm_response.status(), StatusCode::CONFLICT);

        let state_response = router
            .oneshot(get_request("/state", Some(&token)))
            .await
            .unwrap();
        assert_eq!(
            body_json(state_response).await,
            json!({ "state": "ExitDelay" })
        );
    }

    #[tokio::test]
    async fn disarm_from_disarmed_succeeds() {
        let router = app(test_support::state().await);
        let token = register_and_login(&router, "alice", "hunter2").await;

        let disarm_response = router
            .clone()
            .oneshot(post_request("/disarm", Some(&token)))
            .await
            .unwrap();
        assert_eq!(disarm_response.status(), StatusCode::OK);
        assert_eq!(
            body_json(disarm_response).await,
            json!({ "state": "Disarmed" })
        );

        let state_response = router
            .oneshot(get_request("/state", Some(&token)))
            .await
            .unwrap();
        assert_eq!(
            body_json(state_response).await,
            json!({ "state": "Disarmed" })
        );
    }

    #[tokio::test]
    async fn disarm_after_arm_succeeds_and_state_reports_disarmed() {
        let router = app(test_support::state().await);
        let token = register_and_login(&router, "alice", "hunter2").await;

        router
            .clone()
            .oneshot(post_request("/arm", Some(&token)))
            .await
            .unwrap();

        let disarm_response = router
            .clone()
            .oneshot(post_request("/disarm", Some(&token)))
            .await
            .unwrap();
        assert_eq!(disarm_response.status(), StatusCode::OK);
        assert_eq!(
            body_json(disarm_response).await,
            json!({ "state": "Disarmed" })
        );

        let state_response = router
            .oneshot(get_request("/state", Some(&token)))
            .await
            .unwrap();
        assert_eq!(
            body_json(state_response).await,
            json!({ "state": "Disarmed" })
        );
    }

    #[tokio::test]
    async fn arm_and_disarm_without_token_fail() {
        let router = app(test_support::state().await);

        let arm_response = router
            .clone()
            .oneshot(post_request("/arm", None))
            .await
            .unwrap();
        assert_eq!(arm_response.status(), StatusCode::UNAUTHORIZED);

        let disarm_response = router.oneshot(post_request("/disarm", None)).await.unwrap();
        assert_eq!(disarm_response.status(), StatusCode::UNAUTHORIZED);
    }
}
