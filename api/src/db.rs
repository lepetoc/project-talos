use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

#[derive(sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub username: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn insert_and_query_user_by_id() {
        let pool = init_pool("sqlite::memory:").await.unwrap();

        let inserted = sqlx::query("INSERT INTO users (username) VALUES (?)")
            .bind("alice")
            .execute(&pool)
            .await
            .unwrap();

        let user: User = sqlx::query_as("SELECT id, username FROM users WHERE id = ?")
            .bind(inserted.last_insert_rowid())
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(user.username, "alice");
    }
}
