//! Filtered history pages use the (event_type, date) index instead of scanning the table.
use reelix_db::Db;

#[tokio::test]
async fn filtered_history_uses_the_event_date_index() {
    let db = Db::in_memory().await.unwrap();
    // A realistic spread: many grabs and imports, a few failures, then the statistics `PRAGMA optimize` keeps.
    for i in 0..2000_i64 {
        let kind = match i % 20 {
            0 => "download_failed",
            1..=9 => "grabbed",
            _ => "imported",
        };
        sqlx::query("INSERT INTO history (event_type, source_title, date) VALUES (?, 'x', ?)")
            .bind(kind)
            .bind(i)
            .execute(db.pool())
            .await
            .unwrap();
    }
    sqlx::query("ANALYZE").execute(db.pool()).await.unwrap();
    let plan = |sql: &'static str| {
        let pool = db.pool().clone();
        async move {
            let rows: Vec<(i64, i64, i64, String)> = sqlx::query_as(sql)
                .bind("download_failed")
                .fetch_all(&pool)
                .await
                .unwrap();
            rows.into_iter().map(|r| r.3).collect::<Vec<_>>()
        }
    };
    let count = plan("EXPLAIN QUERY PLAN SELECT COUNT(*) FROM history WHERE event_type = ?").await;
    assert!(
        count.iter().any(|d| d.contains("idx_history_event_date")),
        "{count:?}"
    );
    let page = plan(
        "EXPLAIN QUERY PLAN SELECT id FROM history WHERE event_type = ? ORDER BY date DESC, id DESC LIMIT 50",
    )
    .await;
    assert!(
        page.iter().any(|d| d.contains("idx_history_event_date")),
        "{page:?}"
    );
}

#[tokio::test]
async fn file_databases_limit_the_write_ahead_log() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::connect(&format!("sqlite://{}", dir.path().join("r.db").display()))
        .await
        .unwrap();
    let limit: i64 = sqlx::query_scalar("PRAGMA journal_size_limit")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(limit, 64 * 1024 * 1024);
}
