//! Space usage counters and quotas

use sqlx::{SqliteConnection, SqlitePool};

use super::{Drive, get_drive};
use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// Adds `delta` bytes to a space's usage counter (call inside the transaction that adds, replaces or removes file nodes)
pub async fn adjust_usage(conn: &mut SqliteConnection, drive_id: &str, delta: i64) -> AppResult<()> {
    if delta == 0 || drive_id.is_empty() {
        return Ok(());
    }
    let after: Option<(i64,)> = sqlx::query_as("UPDATE drives SET used_bytes = used_bytes + ? WHERE id = ? RETURNING used_bytes")
        .bind(delta)
        .bind(drive_id)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some((n,)) = after
        && n < 0
    {
        // A path that adds files without counting them; the daily recompute puts the counter right, but it shouldn't happen
        tracing::warn!("Space usage of {drive_id} went negative ({n}) after {delta:+}; clamped to 0");
        sqlx::query("UPDATE drives SET used_bytes = 0 WHERE id = ?").bind(drive_id).execute(conn).await?;
    }
    Ok(())
}

/// Recomputes every space's usage counter from the node table (startup and once a day, in case a counter drifted)
pub async fn recompute_usage(st: &AppState) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    sqlx::query("UPDATE drives SET used_bytes = (SELECT COALESCE(SUM(size), 0) FROM nodes WHERE drive_id = drives.id AND kind = 'file')")
        .execute(&st.db)
        .await?;
    Ok(())
}

/// Space used in the user's personal space
pub async fn used_bytes(db: &SqlitePool, user_id: i64) -> AppResult<i64> {
    let (used,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(used_bytes), 0) FROM drives WHERE kind = 'personal' AND owner_id = ?")
        .bind(user_id)
        .fetch_one(db)
        .await?;
    Ok(used)
}

/// Space quota (0 = unlimited): personal spaces use the owner account's quota
pub async fn drive_quota(conn: &mut SqliteConnection, drive: &Drive) -> AppResult<i64> {
    if drive.kind == "personal" {
        let (q,): (i64,) = sqlx::query_as("SELECT COALESCE((SELECT quota_bytes FROM users WHERE id = ?), 0)")
            .bind(drive.owner_id)
            .fetch_one(conn)
            .await?;
        return Ok(q);
    }
    Ok(drive.quota_bytes)
}

/// Checks that adding `extra` bytes to the space won't exceed its quota (including unfinished uploads).
pub async fn check_quota(conn: &mut SqliteConnection, drive_id: &str, extra: i64) -> AppResult<()> {
    if extra <= 0 {
        return Ok(());
    }
    let Some(drive) = get_drive(conn, drive_id).await? else { return Ok(()) };
    let quota = drive_quota(conn, &drive).await?;
    if quota <= 0 {
        return Ok(());
    }
    // Files already there plus uploads still in progress (they were admitted against the quota when they started)
    // Only uploads that received data within the last day hold space: an abandoned one can't block a space for days
    // (every request of an upload moves its expiry to UPLOAD_TTL from then)
    let active_since = crate::util::now() + crate::upload::UPLOAD_TTL - 86400;
    let (pending,): (i64,) =
        sqlx::query_as("SELECT COALESCE(SUM(size), 0) FROM uploads WHERE drive_id = ? AND node_id IS NULL AND expires_at > ?")
            .bind(drive_id)
            .bind(active_since)
            .fetch_one(conn)
            .await?;
    let used = drive.used_bytes + pending;
    if used + extra > quota {
        return Err(AppError::new(axum::http::StatusCode::PAYLOAD_TOO_LARGE, format!("Not enough storage space in \"{}\"", drive.name)).with_code("quota"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn an_upload_without_progress_for_a_day_no_longer_holds_space() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let drive = env.drive_of(&amy.root_id).await;
        // A personal space's quota is its owner's
        sqlx::query("UPDATE users SET quota_bytes = 1000 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        let ts = crate::util::now();
        sqlx::query("INSERT INTO uploads (id, owner_id, parent_id, rel_path, name, size, offset, created_at, expires_at, drive_id) VALUES ('u1', ?, ?, '', 'big.bin', 900, 0, ?, ?, ?)")
            .bind(amy.id)
            .bind(&amy.root_id)
            .bind(ts)
            .bind(ts + crate::upload::UPLOAD_TTL)
            .bind(&drive)
            .execute(&env.st.db)
            .await
            .unwrap();
        let mut c = env.st.db.acquire().await.unwrap();
        assert!(check_quota(&mut c, &drive, 200).await.is_err(), "a fresh upload reserves its size");
        sqlx::query("UPDATE uploads SET expires_at = ? WHERE id = 'u1'").bind(ts + crate::upload::UPLOAD_TTL - 2 * 86400).execute(&mut *c).await.unwrap();
        assert!(check_quota(&mut c, &drive, 200).await.is_ok(), "an upload idle for two days no longer does");
    }

}
