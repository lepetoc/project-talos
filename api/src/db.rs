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
    // Kept for Debug output; callers currently only distinguish UsernameTaken from everything else.
    Other(#[allow(dead_code)] sqlx::Error),
}

impl From<sqlx::Error> for InsertUserError {
    fn from(err: sqlx::Error) -> Self {
        match err.as_database_error() {
            Some(db_err) if db_err.is_unique_violation() => InsertUserError::UsernameTaken,
            _ => InsertUserError::Other(err),
        }
    }
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
}
