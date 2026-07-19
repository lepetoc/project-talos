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

/// Only called from `routes.rs` under the `shelly` feature so far; exercised
/// directly by tests otherwise.
#[cfg_attr(not(feature = "shelly"), allow(dead_code))]
pub async fn get_shelly_gateway_addr(pool: &SqlitePool) -> sqlx::Result<Option<String>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT gateway_addr FROM shelly_config WHERE id = 1")
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(addr,)| addr))
}

/// Only called from `routes.rs` under the `shelly` feature so far; exercised
/// directly by tests otherwise.
#[cfg_attr(not(feature = "shelly"), allow(dead_code))]
pub async fn set_shelly_gateway_addr(pool: &SqlitePool, addr: &str) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO shelly_config (id, gateway_addr) VALUES (1, ?) \
         ON CONFLICT(id) DO UPDATE SET gateway_addr = excluded.gateway_addr",
    )
    .bind(addr)
    .execute(pool)
    .await?;
    Ok(())
}

/// Only called from `routes.rs` and `main.rs` under the `sia_dc09` feature so
/// far; exercised directly by tests otherwise. An incomplete configuration
/// (any of the three columns still null) is treated the same as no
/// configuration at all.
#[cfg_attr(not(feature = "sia_dc09"), allow(dead_code))]
pub async fn get_sia_config(pool: &SqlitePool) -> sqlx::Result<Option<(String, String, String)>> {
    let row: Option<(Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT account, prefix, receiver_addr FROM sia_config WHERE id = 1")
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(
        |(account, prefix, receiver_addr)| match (account, prefix, receiver_addr) {
            (Some(account), Some(prefix), Some(receiver_addr)) => {
                Some((account, prefix, receiver_addr))
            }
            _ => None,
        },
    ))
}

/// Only called from `routes.rs` under the `sia_dc09` feature so far;
/// exercised directly by tests otherwise.
#[cfg_attr(not(feature = "sia_dc09"), allow(dead_code))]
pub async fn set_sia_config(
    pool: &SqlitePool,
    account: &str,
    prefix: &str,
    receiver_addr: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO sia_config (id, account, prefix, receiver_addr) VALUES (1, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET account = excluded.account, \
         prefix = excluded.prefix, receiver_addr = excluded.receiver_addr",
    )
    .bind(account)
    .bind(prefix)
    .bind(receiver_addr)
    .execute(pool)
    .await?;
    Ok(())
}

/// Only called from `main.rs` under the `shelly` feature so far; exercised
/// directly by tests otherwise.
#[cfg_attr(not(feature = "shelly"), allow(dead_code))]
pub async fn load_sensor_mappings(
    pool: &SqlitePool,
) -> sqlx::Result<std::collections::HashMap<String, u32>> {
    let rows: Vec<(String, i64)> = sqlx::query_as("SELECT sensor_id, zone_id FROM sensor_mappings")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(sensor_id, zone_id)| (sensor_id, zone_id as u32))
        .collect())
}

/// The database-backed counterpart to `AlarmHandle::list_sensor_mappings`;
/// routes read the in-memory map instead since that's the live source of
/// truth, so this isn't called from production code yet. Exercised directly
/// by tests.
#[allow(dead_code)]
pub async fn list_sensor_mappings(pool: &SqlitePool) -> sqlx::Result<Vec<(String, u32)>> {
    let rows: Vec<(String, i64)> = sqlx::query_as("SELECT sensor_id, zone_id FROM sensor_mappings")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(sensor_id, zone_id)| (sensor_id, zone_id as u32))
        .collect())
}

/// Only called from `routes.rs` under the `shelly` feature so far; exercised
/// directly by tests otherwise.
#[cfg_attr(not(feature = "shelly"), allow(dead_code))]
pub async fn insert_sensor_mapping(
    pool: &SqlitePool,
    sensor_id: &str,
    zone_id: u32,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO sensor_mappings (sensor_id, zone_id) VALUES (?, ?) \
         ON CONFLICT(sensor_id) DO UPDATE SET zone_id = excluded.zone_id",
    )
    .bind(sensor_id)
    .bind(zone_id as i64)
    .execute(pool)
    .await?;
    Ok(())
}

/// Only called from `routes.rs` under the `shelly` feature so far; exercised
/// directly by tests otherwise.
#[cfg_attr(not(feature = "shelly"), allow(dead_code))]
pub async fn delete_sensor_mapping(pool: &SqlitePool, sensor_id: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM sensor_mappings WHERE sensor_id = ?")
        .bind(sensor_id)
        .execute(pool)
        .await?;
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
    async fn shelly_gateway_addr_starts_null_then_round_trips() {
        let pool = init_pool("sqlite::memory:").await.unwrap();

        assert_eq!(get_shelly_gateway_addr(&pool).await.unwrap(), None);

        set_shelly_gateway_addr(&pool, "192.168.1.50:1010")
            .await
            .unwrap();

        assert_eq!(
            get_shelly_gateway_addr(&pool).await.unwrap(),
            Some("192.168.1.50:1010".to_string())
        );

        set_shelly_gateway_addr(&pool, "192.168.1.51:1010")
            .await
            .unwrap();

        assert_eq!(
            get_shelly_gateway_addr(&pool).await.unwrap(),
            Some("192.168.1.51:1010".to_string())
        );
    }

    #[tokio::test]
    async fn sia_config_is_none_until_all_three_columns_are_set() {
        let pool = init_pool("sqlite::memory:").await.unwrap();

        assert_eq!(get_sia_config(&pool).await.unwrap(), None);

        // Set only one of the three columns directly, leaving the other two
        // null: still treated as unconfigured.
        sqlx::query("UPDATE sia_config SET account = ? WHERE id = 1")
            .bind("1234")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(get_sia_config(&pool).await.unwrap(), None);

        set_sia_config(&pool, "1234", "0", "192.168.1.60:5555")
            .await
            .unwrap();

        assert_eq!(
            get_sia_config(&pool).await.unwrap(),
            Some((
                "1234".to_string(),
                "0".to_string(),
                "192.168.1.60:5555".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn load_sensor_mappings_returns_seeded_rows() {
        let pool = init_pool("sqlite::memory:").await.unwrap();

        sqlx::query("INSERT INTO sensor_mappings (sensor_id, zone_id) VALUES (?, ?)")
            .bind("front-door")
            .bind(1i64)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO sensor_mappings (sensor_id, zone_id) VALUES (?, ?)")
            .bind("back-door")
            .bind(2i64)
            .execute(&pool)
            .await
            .unwrap();

        let mappings = load_sensor_mappings(&pool).await.unwrap();

        assert_eq!(mappings.get("front-door"), Some(&1));
        assert_eq!(mappings.get("back-door"), Some(&2));
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

    #[tokio::test]
    async fn insert_sensor_mapping_then_list_returns_it() {
        let pool = init_pool("sqlite::memory:").await.unwrap();

        insert_sensor_mapping(&pool, "front-door", 1).await.unwrap();

        let mappings = list_sensor_mappings(&pool).await.unwrap();
        assert!(mappings.contains(&("front-door".to_string(), 1)));
    }

    #[tokio::test]
    async fn insert_sensor_mapping_twice_upserts_zone_id() {
        let pool = init_pool("sqlite::memory:").await.unwrap();

        insert_sensor_mapping(&pool, "front-door", 1).await.unwrap();
        insert_sensor_mapping(&pool, "front-door", 2).await.unwrap();

        let mappings = list_sensor_mappings(&pool).await.unwrap();
        assert!(mappings.contains(&("front-door".to_string(), 2)));
        assert!(!mappings.contains(&("front-door".to_string(), 1)));
    }

    #[tokio::test]
    async fn delete_sensor_mapping_removes_it() {
        let pool = init_pool("sqlite::memory:").await.unwrap();

        insert_sensor_mapping(&pool, "front-door", 1).await.unwrap();
        delete_sensor_mapping(&pool, "front-door").await.unwrap();

        let mappings = list_sensor_mappings(&pool).await.unwrap();
        assert!(!mappings.iter().any(|(id, _)| id == "front-door"));
    }

    #[tokio::test]
    async fn delete_sensor_mapping_absent_id_is_not_an_error() {
        let pool = init_pool("sqlite::memory:").await.unwrap();

        delete_sensor_mapping(&pool, "nope").await.unwrap();
    }
}
