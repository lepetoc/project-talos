use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

#[derive(Debug, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    // Looked up by username, not read back from it yet outside tests.
    #[allow(dead_code)]
    pub username: String,
    pub password_hash: String,
}

#[derive(Debug)]
pub enum InsertUserError {
    UsernameTaken,
    Other(sqlx::Error),
}

impl From<sqlx::Error> for InsertUserError {
    fn from(err: sqlx::Error) -> Self {
        match err.as_database_error() {
            Some(db_err) if db_err.is_unique_violation() => InsertUserError::UsernameTaken,
            _ => InsertUserError::Other(err),
        }
    }
}

const DEFAULT_DATABASE_URL: &str = "sqlite://talos.db";

/// Reads the database URL from `TALOS_DATABASE_URL`, the same way the delay
/// durations are read in `timers.rs`: optional, falling back to a default.
pub fn database_url_from_env() -> String {
    std::env::var("TALOS_DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
}

pub async fn init_pool(database_url: &str) -> sqlx::Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}

pub async fn count_users(pool: &SqlitePool) -> sqlx::Result<i64> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await?;
    Ok(count)
}

pub async fn find_user_by_username(
    pool: &SqlitePool,
    username: &str,
) -> sqlx::Result<Option<User>> {
    sqlx::query_as::<_, User>("SELECT id, username, password_hash FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(pool)
        .await
}

pub async fn insert_user(
    pool: &SqlitePool,
    username: &str,
    password_hash: &str,
) -> Result<i64, InsertUserError> {
    let result = sqlx::query("INSERT INTO users (username, password_hash) VALUES (?, ?)")
        .bind(username)
        .bind(password_hash)
        .execute(pool)
        .await?;
    Ok(result.last_insert_rowid())
}

pub(crate) fn zone_kind_to_str(kind: talos_core::ZoneKind) -> &'static str {
    match kind {
        talos_core::ZoneKind::Delay => "Delay",
        talos_core::ZoneKind::Instant => "Instant",
    }
}

pub(crate) fn zone_status_to_str(status: talos_core::ZoneStatus) -> &'static str {
    match status {
        talos_core::ZoneStatus::Clear => "Clear",
        talos_core::ZoneStatus::Triggered => "Triggered",
    }
}

pub(crate) fn parse_zone_kind(raw: &str) -> sqlx::Result<talos_core::ZoneKind> {
    match raw {
        "Delay" => Ok(talos_core::ZoneKind::Delay),
        "Instant" => Ok(talos_core::ZoneKind::Instant),
        other => Err(sqlx::Error::Decode(
            format!("unknown zone kind in database: {other}").into(),
        )),
    }
}

pub async fn insert_zone(
    pool: &SqlitePool,
    id: i64,
    kind: talos_core::ZoneKind,
) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO zones (id, kind) VALUES (?, ?)")
        .bind(id)
        .bind(zone_kind_to_str(kind))
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_zone(pool: &SqlitePool, id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM zones WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_zones(pool: &SqlitePool) -> sqlx::Result<Vec<(i64, talos_core::ZoneKind)>> {
    let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, kind FROM zones")
        .fetch_all(pool)
        .await?;
    rows.into_iter()
        .map(|(id, kind)| parse_zone_kind(&kind).map(|kind| (id, kind)))
        .collect()
}

/// Rebuilds in-memory zone registration from what's persisted in the database.
pub async fn replay_zones(pool: &SqlitePool, alarm: &mut talos_core::Alarm) -> sqlx::Result<()> {
    for (id, kind) in list_zones(pool).await? {
        alarm
            .add_zone(id as u32, kind)
            .map_err(|err| sqlx::Error::Decode(err.to_string().into()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn insert_and_query_user_by_id() {
        let pool = init_pool("sqlite::memory:").await.unwrap();

        let inserted = sqlx::query("INSERT INTO users (username, password_hash) VALUES (?, ?)")
            .bind("alice")
            .bind("hashed-value")
            .execute(&pool)
            .await
            .unwrap();

        let user: User =
            sqlx::query_as("SELECT id, username, password_hash FROM users WHERE id = ?")
                .bind(inserted.last_insert_rowid())
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(user.username, "alice");
    }

    #[tokio::test]
    async fn insert_zone_then_list_zones_returns_it() {
        let pool = init_pool("sqlite::memory:").await.unwrap();

        insert_zone(&pool, 1, talos_core::ZoneKind::Delay)
            .await
            .unwrap();
        insert_zone(&pool, 2, talos_core::ZoneKind::Instant)
            .await
            .unwrap();

        let mut zones = list_zones(&pool).await.unwrap();
        zones.sort_by_key(|(id, _)| *id);
        assert_eq!(
            zones,
            vec![
                (1, talos_core::ZoneKind::Delay),
                (2, talos_core::ZoneKind::Instant)
            ]
        );
    }

    #[tokio::test]
    async fn replay_zones_populates_alarm_from_database() {
        let pool = init_pool("sqlite::memory:").await.unwrap();

        insert_zone(&pool, 1, talos_core::ZoneKind::Delay)
            .await
            .unwrap();
        insert_zone(&pool, 2, talos_core::ZoneKind::Instant)
            .await
            .unwrap();

        let mut alarm = talos_core::Alarm::new();
        replay_zones(&pool, &mut alarm).await.unwrap();

        let mut zones = alarm.list_zones();
        zones.sort_by_key(|(id, _, _)| *id);
        assert_eq!(
            zones,
            vec![
                (
                    1,
                    talos_core::ZoneKind::Delay,
                    talos_core::ZoneStatus::Clear
                ),
                (
                    2,
                    talos_core::ZoneKind::Instant,
                    talos_core::ZoneStatus::Clear
                ),
            ]
        );
    }
}
