//! Migration 0021 gives VIEW_SYSTEM (bit 22) to accounts that already hold the four manage rights,
//! and to nobody else (RFX-84). The migration has run on the fresh database, so the test inserts
//! accounts as they were before it and runs its SQL again.

use reelix_db::Db;
use sqlx::Row;

const VIEW_SYSTEM: i64 = 1 << 22;
const MIGRATION: &str = include_str!("../../../migrations/sqlite/0021_view_system.sql");

#[tokio::test]
async fn managers_get_view_system_and_others_do_not() {
    let db = Db::in_memory().await.unwrap();
    // Old Manager preset (manage rights 1-4 plus power-user bits), only some manage rights, a
    // plain user and an admin.
    let accounts: [(&str, i64); 4] = [
        ("manager", 0b11_1111_1111_1111_1111_1110),
        ("library_only", 0b10 | (1 << 5)),
        ("user", (1 << 5) | (1 << 6) | (1 << 13)),
        ("admin", 1),
    ];
    for (name, bits) in accounts {
        sqlx::query("INSERT INTO users (username, permissions, created_at, updated_at) VALUES (?, ?, 0, 0)")
            .bind(name)
            .bind(bits & !VIEW_SYSTEM)
            .execute(db.pool())
            .await
            .unwrap();
    }
    sqlx::raw_sql(MIGRATION).execute(db.pool()).await.unwrap();
    for (name, bits) in accounts {
        let row = sqlx::query("SELECT permissions FROM users WHERE username = ?")
            .bind(name)
            .fetch_one(db.pool())
            .await
            .unwrap();
        let after: i64 = row.get("permissions");
        let expected = if name == "manager" {
            (bits & !VIEW_SYSTEM) | VIEW_SYSTEM
        } else {
            bits & !VIEW_SYSTEM
        };
        assert_eq!(after, expected, "{name}");
    }
}
