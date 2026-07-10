use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::extract::{FromRequestParts, OptionalFromRequestParts};
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use jsonwebtoken::{
    decode, encode, get_current_timestamp, Algorithm, DecodingKey, EncodingKey, Header, Validation,
};
use serde::{Deserialize, Serialize};

use crate::AppState;

const ONE_DAY_SECS: u64 = 24 * 60 * 60;

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
}

pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default().hash_password(password.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> Result<bool, argon2::password_hash::Error> {
    let parsed_hash = PasswordHash::new(hash)?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok())
}

/// Reads the JWT signing secret from `TALOS_JWT_SECRET`. There is no default or
/// hardcoded fallback: if the variable is unset, the caller should refuse to start.
pub fn jwt_secret_from_env() -> Result<String, String> {
    std::env::var("TALOS_JWT_SECRET")
        .map_err(|_| "TALOS_JWT_SECRET environment variable must be set".to_string())
}

pub fn create_token(user_id: i64, secret: &str) -> Result<String, jsonwebtoken::errors::Error> {
    let claims = Claims {
        sub: user_id.to_string(),
        exp: (get_current_timestamp() + ONE_DAY_SECS) as usize,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

fn decode_claims(token: &str, secret: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::new(Algorithm::HS256),
    )?;
    Ok(data.claims)
}

/// The authenticated user extracted from a valid `Authorization: Bearer <jwt>` header.
// Not read by any handler yet; the bootstrap check only needs presence/absence.
#[allow(dead_code)]
pub struct AuthUser {
    pub user_id: i64,
}

impl AuthUser {
    /// Validates a raw JWT (no `Bearer ` prefix) against the same secret and
    /// logic used for header-based authentication. Used by the `/ws` handler,
    /// which authenticates via the first message instead of a header.
    pub fn from_token(token: &str, secret: &str) -> Result<Self, StatusCode> {
        let claims = decode_claims(token, secret).map_err(|_| StatusCode::UNAUTHORIZED)?;
        let user_id = claims
            .sub
            .parse::<i64>()
            .map_err(|_| StatusCode::UNAUTHORIZED)?;
        Ok(AuthUser { user_id })
    }

    fn from_headers(headers: &axum::http::HeaderMap, secret: &str) -> Result<Self, StatusCode> {
        let value = headers
            .get(header::AUTHORIZATION)
            .ok_or(StatusCode::UNAUTHORIZED)?;
        let value = value.to_str().map_err(|_| StatusCode::UNAUTHORIZED)?;
        let token = value
            .strip_prefix("Bearer ")
            .ok_or(StatusCode::UNAUTHORIZED)?;
        Self::from_token(token, secret)
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Self::from_headers(&parts.headers, &state.jwt_secret)
    }
}

/// Allows `Option<AuthUser>` to be used where a token may or may not be present,
/// e.g. the bootstrap path of `/auth/register`.
impl OptionalFromRequestParts<AppState> for AuthUser {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, Self::Rejection> {
        if parts.headers.get(header::AUTHORIZATION).is_none() {
            return Ok(None);
        }
        Self::from_headers(&parts.headers, &state.jwt_secret).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;
    use axum::http::Request;

    fn parts_with_header(auth_header: Option<&str>) -> Parts {
        let mut builder = Request::builder().uri("/");
        if let Some(value) = auth_header {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let (parts, ()) = builder.body(()).unwrap().into_parts();
        parts
    }

    #[test]
    fn hashing_then_verifying_the_same_password_succeeds() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &hash).unwrap());
        assert!(!verify_password("wrong password", &hash).unwrap());
    }

    #[tokio::test]
    async fn missing_authorization_header_is_rejected() {
        let state = test_support::state().await;
        let mut parts = parts_with_header(None);

        let result =
            <AuthUser as FromRequestParts<AppState>>::from_request_parts(&mut parts, &state).await;

        assert_eq!(result.err(), Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn garbage_token_is_rejected() {
        let state = test_support::state().await;
        let mut parts = parts_with_header(Some("Bearer not-a-real-token"));

        let result =
            <AuthUser as FromRequestParts<AppState>>::from_request_parts(&mut parts, &state).await;

        assert_eq!(result.err(), Some(StatusCode::UNAUTHORIZED));
    }
}
