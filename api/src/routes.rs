use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{error, info, warn};

use crate::auth::{self, AuthUser};
use crate::db;
use crate::AppState;

const WS_AUTH_TIMEOUT: Duration = Duration::from_secs(5);

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

#[cfg(feature = "shelly")]
#[derive(Serialize)]
pub struct ShellyConfigResponse {
    gateway_addr: Option<String>,
}

#[cfg(feature = "shelly")]
#[derive(Deserialize)]
pub struct ShellyConfigRequest {
    gateway_addr: String,
}

#[cfg(feature = "shelly")]
#[derive(Serialize)]
pub struct SensorMappingResponse {
    sensor_id: String,
    zone_id: u32,
}

#[cfg(feature = "shelly")]
#[derive(Deserialize)]
pub struct SensorMappingRequest {
    sensor_id: String,
    zone_id: u32,
}

#[cfg(feature = "sia_dc09")]
#[derive(Serialize)]
pub struct SiaConfigResponse {
    account: Option<String>,
    prefix: Option<String>,
    receiver_addr: Option<String>,
}

#[cfg(feature = "sia_dc09")]
#[derive(Deserialize)]
pub struct SiaConfigRequest {
    account: String,
    prefix: String,
    receiver_addr: String,
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
    let router = Router::new()
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/zones", post(create_zone).get(list_zones))
        .route("/zones/{id}", delete(delete_zone))
        .route("/arm", post(arm))
        .route("/disarm", post(disarm))
        .route("/state", get(get_state))
        .route("/ws", get(ws_handler));

    #[cfg(feature = "shelly")]
    let router = router
        .route(
            "/modules/shelly/config",
            get(get_shelly_config).put(put_shelly_config),
        )
        .route(
            "/modules/shelly/sensors",
            get(list_sensor_mappings).post(add_sensor_mapping),
        )
        .route(
            "/modules/shelly/sensors/{sensor_id}",
            delete(remove_sensor_mapping),
        );

    #[cfg(feature = "sia_dc09")]
    let router = router.route(
        "/modules/sia/config",
        get(get_sia_config).put(put_sia_config),
    );

    router
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
    let user_count = db::count_users(&state.pool).await.map_err(|err| {
        error!(%err, "failed to count users");
        ApiError::Internal
    })?;

    let is_bootstrap = user_count == 0;
    if !is_bootstrap && auth.is_none() {
        return Err(ApiError::Unauthorized("authentication required"));
    }

    let password_hash = auth::hash_password(&payload.password).map_err(|err| {
        error!(%err, "failed to hash password");
        ApiError::Internal
    })?;

    match db::insert_user(&state.pool, &payload.username, &password_hash).await {
        Ok(_) => {
            if is_bootstrap {
                warn!(
                    username = %payload.username,
                    "first account created via unauthenticated bootstrap"
                );
            } else {
                info!(username = %payload.username, "registered new account");
            }
            Ok(StatusCode::CREATED)
        }
        Err(db::InsertUserError::UsernameTaken) => Err(ApiError::Conflict),
        Err(db::InsertUserError::Other(err)) => {
            error!(%err, "failed to insert user");
            Err(ApiError::Internal)
        }
    }
}

async fn login(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    let user = db::find_user_by_username(&state.pool, &payload.username)
        .await
        .map_err(|err| {
            error!(%err, "failed to look up user by username");
            ApiError::Internal
        })?;
    let user = match user {
        Some(user) => user,
        None => {
            warn!(username = %payload.username, "login failed: unknown username");
            return Err(ApiError::Unauthorized(INVALID_CREDENTIALS));
        }
    };

    let valid = auth::verify_password(&payload.password, &user.password_hash).map_err(|err| {
        error!(%err, "failed to verify password");
        ApiError::Internal
    })?;
    if !valid {
        warn!(username = %payload.username, "login failed: wrong password");
        return Err(ApiError::Unauthorized(INVALID_CREDENTIALS));
    }

    let token = auth::create_token(user.id, &state.jwt_secret).map_err(|err| {
        error!(%err, "failed to create token");
        ApiError::Internal
    })?;
    info!(username = %payload.username, "login succeeded");
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

    if let Err(err) = db::insert_zone(&state.pool, payload.id as i64, kind).await {
        error!(zone_id = payload.id, %err, "failed to persist new zone; rolling back in-memory add");
        if let Err(rollback_err) = state.alarm.lock().unwrap().remove_zone(payload.id) {
            error!(
                zone_id = payload.id,
                %rollback_err,
                "failed to roll back in-memory zone add after database insert failure; in-memory and database state are now desynchronized"
            );
        }
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

        match kind {
            Some(kind) => kind,
            None => {
                error!(
                    zone_id = id,
                    "zone kind missing immediately after a successful remove_zone; in-memory state is desynchronized"
                );
                return Err(ApiError::Internal);
            }
        }
    };

    if let Err(err) = db::delete_zone(&state.pool, id as i64).await {
        error!(zone_id = id, %err, "failed to persist zone deletion; rolling back in-memory removal");
        if let Err(rollback_err) = state.alarm.lock().unwrap().add_zone(id, kind) {
            error!(
                zone_id = id,
                %rollback_err,
                "failed to roll back in-memory zone removal after database delete failure; in-memory and database state are now desynchronized"
            );
        }
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
    let new_state = alarm.state();
    let zones = alarm.list_zones();
    let _ = state.tx.send(new_state);
    log_state_transition(new_state);
    notify_actioneurs(&state, new_state, &zones);
    Ok(Json(StateResponse {
        state: state_to_str(new_state).to_string(),
    }))
}

async fn disarm(State(state): State<AppState>, _auth: AuthUser) -> Json<StateResponse> {
    let mut alarm = state.alarm.lock().unwrap();
    alarm.disarm();
    let new_state = alarm.state();
    let zones = alarm.list_zones();
    let _ = state.tx.send(new_state);
    log_state_transition(new_state);
    notify_actioneurs(&state, new_state, &zones);
    Json(StateResponse {
        state: state_to_str(new_state).to_string(),
    })
}

fn notify_actioneurs(
    state: &AppState,
    new_state: talos_core::State,
    zones: &[(u32, talos_core::ZoneKind, talos_core::ZoneStatus)],
) {
    let mut actioneurs = state.actioneurs.lock().unwrap();
    for actioneur in actioneurs.iter_mut() {
        actioneur.on_state_change(new_state, zones);
    }
}

fn log_state_transition(new_state: talos_core::State) {
    if new_state == talos_core::State::Triggered {
        warn!(state = %state_to_str(new_state), "alarm state transition");
    } else {
        info!(state = %state_to_str(new_state), "alarm state transition");
    }
}

async fn get_state(State(state): State<AppState>, _auth: AuthUser) -> Json<StateResponse> {
    let alarm = state.alarm.lock().unwrap();
    Json(StateResponse {
        state: state_to_str(alarm.state()).to_string(),
    })
}

#[cfg(feature = "shelly")]
async fn get_shelly_config(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<ShellyConfigResponse>, ApiError> {
    let gateway_addr = db::get_shelly_gateway_addr(&state.pool)
        .await
        .map_err(|err| {
            error!(%err, "failed to load shelly gateway address");
            ApiError::Internal
        })?;
    Ok(Json(ShellyConfigResponse { gateway_addr }))
}

#[cfg(feature = "shelly")]
async fn put_shelly_config(
    State(state): State<AppState>,
    _auth: AuthUser,
    Json(payload): Json<ShellyConfigRequest>,
) -> Result<StatusCode, ApiError> {
    db::set_shelly_gateway_addr(&state.pool, &payload.gateway_addr)
        .await
        .map_err(|err| {
            error!(%err, "failed to persist shelly gateway address");
            ApiError::Internal
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(feature = "shelly")]
async fn list_sensor_mappings(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Json<Vec<SensorMappingResponse>> {
    Json(
        state
            .alarm_handle
            .list_sensor_mappings()
            .into_iter()
            .map(|(sensor_id, zone_id)| SensorMappingResponse { sensor_id, zone_id })
            .collect(),
    )
}

#[cfg(feature = "shelly")]
async fn add_sensor_mapping(
    State(state): State<AppState>,
    _auth: AuthUser,
    Json(payload): Json<SensorMappingRequest>,
) -> Result<StatusCode, ApiError> {
    let zone_exists = state
        .alarm
        .lock()
        .unwrap()
        .list_zones()
        .into_iter()
        .any(|(zone_id, _, _)| zone_id == payload.zone_id);
    if !zone_exists {
        return Err(ApiError::BadRequest("zone does not exist"));
    }

    db::insert_sensor_mapping(&state.pool, &payload.sensor_id, payload.zone_id)
        .await
        .map_err(|err| {
            error!(%err, "failed to persist sensor mapping");
            ApiError::Internal
        })?;
    state
        .alarm_handle
        .add_sensor_mapping(payload.sensor_id, payload.zone_id);

    Ok(StatusCode::CREATED)
}

#[cfg(feature = "shelly")]
async fn remove_sensor_mapping(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(sensor_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    db::delete_sensor_mapping(&state.pool, &sensor_id)
        .await
        .map_err(|err| {
            error!(%err, "failed to persist sensor mapping removal");
            ApiError::Internal
        })?;
    state.alarm_handle.remove_sensor_mapping(&sensor_id);

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(feature = "sia_dc09")]
async fn get_sia_config(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<SiaConfigResponse>, ApiError> {
    let config = db::get_sia_config(&state.pool).await.map_err(|err| {
        error!(%err, "failed to load sia config");
        ApiError::Internal
    })?;
    let (account, prefix, receiver_addr) = match config {
        Some((account, prefix, receiver_addr)) => {
            (Some(account), Some(prefix), Some(receiver_addr))
        }
        None => (None, None, None),
    };
    Ok(Json(SiaConfigResponse {
        account,
        prefix,
        receiver_addr,
    }))
}

#[cfg(feature = "sia_dc09")]
async fn put_sia_config(
    State(state): State<AppState>,
    _auth: AuthUser,
    Json(payload): Json<SiaConfigRequest>,
) -> Result<StatusCode, ApiError> {
    db::set_sia_config(
        &state.pool,
        &payload.account,
        &payload.prefix,
        &payload.receiver_addr,
    )
    .await
    .map_err(|err| {
        error!(%err, "failed to persist sia config");
        ApiError::Internal
    })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Upgrades to a WebSocket connection. Unlike the other routes, this one has
/// no `AuthUser` extractor: browsers cannot set an `Authorization` header
/// when opening a WebSocket, so authentication instead happens over the
/// socket itself once connected, in `handle_socket`.
async fn ws_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let Ok(Some(Ok(Message::Text(token)))) =
        tokio::time::timeout(WS_AUTH_TIMEOUT, socket.recv()).await
    else {
        return;
    };

    if auth::AuthUser::from_token(&token, &state.jwt_secret).is_err() {
        return;
    }

    let current_state = {
        let alarm = state.alarm.lock().unwrap();
        state_to_str(alarm.state()).to_string()
    };
    if socket
        .send(Message::Text(
            json!({ "state": current_state }).to_string().into(),
        ))
        .await
        .is_err()
    {
        return;
    }

    let mut rx = state.tx.subscribe();
    loop {
        match rx.recv().await {
            Ok(new_state) => {
                let message = json!({ "state": state_to_str(new_state) }).to_string();
                if socket.send(Message::Text(message.into())).await.is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
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

    #[cfg(any(feature = "shelly", feature = "sia_dc09"))]
    fn put_json_request(uri: &str, body: serde_json::Value, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("PUT")
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
    async fn arm_sends_new_state_on_channel() {
        let state = test_support::state().await;
        let mut rx = state.tx.subscribe();
        let router = app(state.clone());
        let token = register_and_login(&router, "alice", "hunter2").await;

        let response = router
            .oneshot(post_request("/arm", Some(&token)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        assert_eq!(rx.recv().await.unwrap(), talos_core::State::ExitDelay);
    }

    #[tokio::test]
    async fn disarm_sends_new_state_on_channel() {
        let state = test_support::state().await;
        let mut rx = state.tx.subscribe();
        let router = app(state.clone());
        let token = register_and_login(&router, "alice", "hunter2").await;

        router
            .clone()
            .oneshot(post_request("/arm", Some(&token)))
            .await
            .unwrap();
        assert_eq!(rx.recv().await.unwrap(), talos_core::State::ExitDelay);

        let response = router
            .oneshot(post_request("/disarm", Some(&token)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        assert_eq!(rx.recv().await.unwrap(), talos_core::State::Disarmed);
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

    #[cfg(feature = "shelly")]
    #[tokio::test]
    async fn shelly_config_round_trips_and_requires_token() {
        let router = app(test_support::state().await);
        let token = register_and_login(&router, "alice", "hunter2").await;

        let get_unauthorized = router
            .clone()
            .oneshot(get_request("/modules/shelly/config", None))
            .await
            .unwrap();
        assert_eq!(get_unauthorized.status(), StatusCode::UNAUTHORIZED);

        let put_unauthorized = router
            .clone()
            .oneshot(put_json_request(
                "/modules/shelly/config",
                json!({ "gateway_addr": "192.168.1.50:1010" }),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(put_unauthorized.status(), StatusCode::UNAUTHORIZED);

        let initial = router
            .clone()
            .oneshot(get_request("/modules/shelly/config", Some(&token)))
            .await
            .unwrap();
        assert_eq!(initial.status(), StatusCode::OK);
        assert_eq!(body_json(initial).await, json!({ "gateway_addr": null }));

        let put_response = router
            .clone()
            .oneshot(put_json_request(
                "/modules/shelly/config",
                json!({ "gateway_addr": "192.168.1.50:1010" }),
                Some(&token),
            ))
            .await
            .unwrap();
        assert_eq!(put_response.status(), StatusCode::NO_CONTENT);

        let after = router
            .oneshot(get_request("/modules/shelly/config", Some(&token)))
            .await
            .unwrap();
        assert_eq!(after.status(), StatusCode::OK);
        assert_eq!(
            body_json(after).await,
            json!({ "gateway_addr": "192.168.1.50:1010" })
        );
    }

    #[cfg(not(feature = "shelly"))]
    #[tokio::test]
    async fn shelly_config_route_absent_without_feature() {
        let router = app(test_support::state().await);

        let response = router
            .oneshot(get_request("/modules/shelly/config", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[cfg(feature = "shelly")]
    #[tokio::test]
    async fn sensor_mapping_routes_require_token() {
        let router = app(test_support::state().await);

        let list_unauthorized = router
            .clone()
            .oneshot(get_request("/modules/shelly/sensors", None))
            .await
            .unwrap();
        assert_eq!(list_unauthorized.status(), StatusCode::UNAUTHORIZED);

        let add_unauthorized = router
            .clone()
            .oneshot(json_request(
                "/modules/shelly/sensors",
                json!({ "sensor_id": "front-door", "zone_id": 1 }),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(add_unauthorized.status(), StatusCode::UNAUTHORIZED);

        let remove_unauthorized = router
            .oneshot(delete_request("/modules/shelly/sensors/front-door", None))
            .await
            .unwrap();
        assert_eq!(remove_unauthorized.status(), StatusCode::UNAUTHORIZED);
    }

    #[cfg(feature = "shelly")]
    #[tokio::test]
    async fn add_sensor_mapping_with_unknown_zone_bad_request() {
        let router = app(test_support::state().await);
        let token = register_and_login(&router, "alice", "hunter2").await;

        let response = router
            .oneshot(json_request(
                "/modules/shelly/sensors",
                json!({ "sensor_id": "front-door", "zone_id": 1 }),
                Some(&token),
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[cfg(feature = "shelly")]
    #[tokio::test]
    async fn add_sensor_mapping_then_report_affects_correct_zone_without_restart() {
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

        let add_response = router
            .clone()
            .oneshot(json_request(
                "/modules/shelly/sensors",
                json!({ "sensor_id": "front-door", "zone_id": 1 }),
                Some(&token),
            ))
            .await
            .unwrap();
        assert_eq!(add_response.status(), StatusCode::CREATED);

        let list_response = router
            .clone()
            .oneshot(get_request("/modules/shelly/sensors", Some(&token)))
            .await
            .unwrap();
        assert_eq!(
            body_json(list_response).await,
            json!([{ "sensor_id": "front-door", "zone_id": 1 }])
        );

        state
            .alarm_handle
            .report("front-door", modules::Reading::Triggered)
            .unwrap();

        let zones_response = router
            .oneshot(get_request("/zones", Some(&token)))
            .await
            .unwrap();
        assert_eq!(
            body_json(zones_response).await,
            json!([{ "id": 1, "kind": "Instant", "status": "Triggered" }])
        );
    }

    #[cfg(feature = "shelly")]
    #[tokio::test]
    async fn remove_sensor_mapping_then_report_returns_unknown_sensor() {
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
        router
            .clone()
            .oneshot(json_request(
                "/modules/shelly/sensors",
                json!({ "sensor_id": "front-door", "zone_id": 1 }),
                Some(&token),
            ))
            .await
            .unwrap();

        let remove_response = router
            .oneshot(delete_request(
                "/modules/shelly/sensors/front-door",
                Some(&token),
            ))
            .await
            .unwrap();
        assert_eq!(remove_response.status(), StatusCode::NO_CONTENT);

        assert_eq!(
            state
                .alarm_handle
                .report("front-door", modules::Reading::Triggered),
            Err(modules::ReportError::UnknownSensor(
                "front-door".to_string()
            ))
        );
    }

    #[cfg(feature = "shelly")]
    #[tokio::test]
    async fn remove_nonexistent_sensor_mapping_still_no_content() {
        let router = app(test_support::state().await);
        let token = register_and_login(&router, "alice", "hunter2").await;

        let response = router
            .oneshot(delete_request("/modules/shelly/sensors/nope", Some(&token)))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[cfg(not(feature = "shelly"))]
    #[tokio::test]
    async fn shelly_sensor_routes_absent_without_feature() {
        let router = app(test_support::state().await);

        let response = router
            .oneshot(get_request("/modules/shelly/sensors", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[cfg(feature = "sia_dc09")]
    #[tokio::test]
    async fn sia_config_round_trips_and_requires_token() {
        let router = app(test_support::state().await);
        let token = register_and_login(&router, "alice", "hunter2").await;

        let get_unauthorized = router
            .clone()
            .oneshot(get_request("/modules/sia/config", None))
            .await
            .unwrap();
        assert_eq!(get_unauthorized.status(), StatusCode::UNAUTHORIZED);

        let put_unauthorized = router
            .clone()
            .oneshot(put_json_request(
                "/modules/sia/config",
                json!({
                    "account": "1234",
                    "prefix": "0",
                    "receiver_addr": "192.168.1.60:5555"
                }),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(put_unauthorized.status(), StatusCode::UNAUTHORIZED);

        let initial = router
            .clone()
            .oneshot(get_request("/modules/sia/config", Some(&token)))
            .await
            .unwrap();
        assert_eq!(initial.status(), StatusCode::OK);
        assert_eq!(
            body_json(initial).await,
            json!({ "account": null, "prefix": null, "receiver_addr": null })
        );

        let put_response = router
            .clone()
            .oneshot(put_json_request(
                "/modules/sia/config",
                json!({
                    "account": "1234",
                    "prefix": "0",
                    "receiver_addr": "192.168.1.60:5555"
                }),
                Some(&token),
            ))
            .await
            .unwrap();
        assert_eq!(put_response.status(), StatusCode::NO_CONTENT);

        let after = router
            .oneshot(get_request("/modules/sia/config", Some(&token)))
            .await
            .unwrap();
        assert_eq!(after.status(), StatusCode::OK);
        assert_eq!(
            body_json(after).await,
            json!({
                "account": "1234",
                "prefix": "0",
                "receiver_addr": "192.168.1.60:5555"
            })
        );
    }

    #[cfg(not(feature = "sia_dc09"))]
    #[tokio::test]
    async fn sia_config_route_absent_without_feature() {
        let router = app(test_support::state().await);

        let response = router
            .oneshot(get_request("/modules/sia/config", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // `oneshot` cannot drive a WebSocket upgrade, so these tests bind the real
    // app to an ephemeral local port and speak WebSocket (via
    // `tokio-tungstenite`) and plain HTTP (hand-rolled over `TcpStream`) to it.
    use futures_util::{SinkExt, StreamExt};
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    async fn spawn_test_server() -> SocketAddr {
        let state = test_support::state().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app(state);
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        addr
    }

    async fn http_request(
        addr: SocketAddr,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (u16, serde_json::Value) {
        let mut stream = TcpStream::connect(addr).await.unwrap();

        let body_bytes = body.map(|value| value.to_string()).unwrap_or_default();
        let mut request =
            format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
        if let Some(token) = token {
            request.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        request.push_str("Content-Type: application/json\r\n");
        request.push_str(&format!("Content-Length: {}\r\n\r\n", body_bytes.len()));
        request.push_str(&body_bytes);

        stream.write_all(request.as_bytes()).await.unwrap();

        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();

        let mut parts = response.splitn(2, "\r\n\r\n");
        let head = parts.next().unwrap();
        let body_str = parts.next().unwrap_or("");

        let status = head
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();

        let body_json = if body_str.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(body_str).unwrap()
        };

        (status, body_json)
    }

    async fn register_and_login_over_http(
        addr: SocketAddr,
        username: &str,
        password: &str,
    ) -> String {
        let (status, _) = http_request(
            addr,
            "POST",
            "/auth/register",
            None,
            Some(json!({ "username": username, "password": password })),
        )
        .await;
        assert_eq!(status, 201);

        let (status, body) = http_request(
            addr,
            "POST",
            "/auth/login",
            None,
            Some(json!({ "username": username, "password": password })),
        )
        .await;
        assert_eq!(status, 200);

        body["token"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn ws_with_invalid_token_closes_without_message() {
        let addr = spawn_test_server().await;

        let (mut ws_stream, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .unwrap();
        ws_stream
            .send(WsMessage::Text("not-a-real-token".into()))
            .await
            .unwrap();

        let received = tokio::time::timeout(Duration::from_secs(2), ws_stream.next())
            .await
            .expect("connection should close promptly after an invalid token");

        if let Some(Ok(WsMessage::Text(_) | WsMessage::Binary(_))) = received {
            panic!("server must not send any message for an invalid token");
        }
    }

    #[tokio::test]
    async fn ws_with_valid_token_streams_state_updates() {
        let addr = spawn_test_server().await;
        let token = register_and_login_over_http(addr, "alice", "hunter2").await;

        let (mut ws_stream, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .unwrap();
        ws_stream
            .send(WsMessage::Text(token.clone().into()))
            .await
            .unwrap();

        let first = ws_stream.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&first.into_text().unwrap()).unwrap(),
            json!({ "state": "Disarmed" })
        );

        let (status, _) = http_request(addr, "POST", "/arm", Some(&token), None).await;
        assert_eq!(status, 200);

        let second = tokio::time::timeout(Duration::from_secs(2), ws_stream.next())
            .await
            .expect("expected a state update after arming")
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&second.into_text().unwrap()).unwrap(),
            json!({ "state": "ExitDelay" })
        );
    }
}
