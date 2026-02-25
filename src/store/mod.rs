use anyhow::Result;
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct UserRecord {
    pub username: String,
    pub first_seen: i64,
    pub last_seen: i64,
    pub is_admin: i64, // SQLite stores bools as 0/1
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub async fn new(path: &str) -> Result<Self> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}?mode=rwc", path))?
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS users (
                username   TEXT PRIMARY KEY,
                first_seen INTEGER NOT NULL,
                last_seen  INTEGER NOT NULL,
                is_admin   INTEGER DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT OR IGNORE INTO settings (key, value) VALUES ('max_users', '50');
            "#,
        )
        .execute(&pool)
        .await?;

        Ok(Store { pool })
    }

    pub async fn touch(&self, username: &str, is_admin: bool) -> Result<()> {
        let now = now_ms();
        let admin = is_admin as i64;
        sqlx::query(
            r#"
            INSERT INTO users (username, first_seen, last_seen, is_admin)
            VALUES (?1, ?2, ?2, ?3)
            ON CONFLICT(username) DO UPDATE SET
                last_seen = excluded.last_seen,
                is_admin  = excluded.is_admin
            "#,
        )
        .bind(username)
        .bind(now)
        .bind(admin)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_users(&self) -> Result<Vec<UserRecord>> {
        let rows = sqlx::query_as::<_, UserRecord>(
            "SELECT username, first_seen, last_seen, is_admin FROM users ORDER BY last_seen DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn delete_user(&self, username: &str) -> Result<()> {
        sqlx::query("DELETE FROM users WHERE username = ?")
            .bind(username)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn user_count(&self) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    pub async fn get_setting(&self, key: &str, default: &str) -> String {
        sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| default.to_string())
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
            .bind(key)
            .bind(value)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}