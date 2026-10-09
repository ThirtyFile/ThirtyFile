//! The hourly maintenance: the trash, earlier versions, the history of background jobs, expired uploads, notifications
//! and the logs.

use std::time::Duration;

use crate::{auth, backups, db, logs, nodes, notify, state::AppState, thumbnails, tree, upload, util, versions};

pub fn spawn_maintenance(st: AppState, trash_days: i64) {
    // Content of spaces deleted while the server stopped before it was all removed
    tree::purge_detached_later(&st);
    util::supervise("maintenance", move |_| maintain(st.clone(), trash_days));
}

async fn maintain(st: AppState, trash_days: i64) {
    {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        let mut hours: u32 = 0;
        loop {
            tick.tick().await;
            hours += 1;
            if hours.is_multiple_of(24) {
                db::optimize(&st.db).await;
            }
            // At the start (the first tick is at once), then daily: space the database and the thumbnails no longer use
            if hours % 24 == 1 {
                db::shrink(&st.db, &st.write_lock).await;
                thumbnails::sweep(&st).await;
            }
            // Once a day: put the usage counters back in step with the node table, should one ever drift
            if hours.is_multiple_of(24)
                && let Err(e) = tree::recompute_usage(&st).await
            {
                tracing::warn!("Couldn't recompute space usage: {}", e.message);
            }
            if trash_days > 0 {
                match nodes::purge_expired_trash(&st, trash_days).await {
                    Ok(n) if n > 0 => tracing::info!("Automatically purged {n} expired trash items"),
                    Err(e) => tracing::warn!("Failed to purge the trash: {}", e.message),
                    _ => {}
                }
            }
            // Spaces that are almost full and access that ends soon (#64)
            if let Err(e) = notify::check(&st).await {
                tracing::warn!("Couldn't check for notifications: {}", e.message);
            }
            match versions::prune(&st).await {
                Ok(n) if n > 0 => tracing::info!("Removed {n} earlier versions of files that are no longer kept"),
                Err(e) => tracing::warn!("Couldn't remove earlier versions of files: {}", e.message),
                _ => {}
            }
            // The history of backup, replica and move jobs
            match backups::runner::trim_histories(&st).await {
                Ok(n) if n > 0 => tracing::info!("Removed {n} finished jobs past the history kept"),
                Err(e) => tracing::warn!("Couldn't remove old finished jobs: {}", e.message),
                _ => {}
            }
            if let Err(e) = upload::purge_expired(&st).await {
                tracing::warn!("Failed to clean up expired uploads: {}", e.message);
            }
            if let Err(e) = upload::clean_tmp(&st).await {
                tracing::warn!("Couldn't clean the temporary directory: {}", e.message);
            }
            auth::prune_login_failures(&st);
            logs::prune_share_views(&st);
            logs::daily_archive(&st).await;
            let _w = st.write_lock.lock().await;
            // Sessions that expired, or ended for being idle
            let _ = sqlx::query("DELETE FROM sessions WHERE expires_at < ? OR COALESCE(last_used_at, created_at) < ?")
                .bind(util::now())
                .bind(auth::idle_before())
                .execute(&st.db)
                .await;
        }
    }
}
