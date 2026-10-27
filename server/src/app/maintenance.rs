//! The hourly maintenance: the trash, earlier versions, the history of background jobs, expired uploads, notifications
//! and the logs.

use std::{
    panic::AssertUnwindSafe,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use futures_util::FutureExt;

use crate::{auth, backups, db, logs, nodes, notify, state::AppState, thumbnails, tree, upload, util, versions};

pub fn spawn_maintenance(st: AppState, trash_days: i64) {
    // Content of spaces deleted while the server stopped before it was all removed
    tree::purge_detached_later(&st);
    // The hours counted go on across a restart after a panic: the daily work keeps its day, and the work done at the
    // start isn't done again each time
    let hours = Arc::new(AtomicU32::new(0));
    util::supervise("maintenance", move |_| maintain(st.clone(), trash_days, hours.clone()));
}

/// One step of the maintenance: a step that panics is reported, and the steps after it still run
async fn step(name: &str, work: impl Future<Output = ()>) {
    if AssertUnwindSafe(work).catch_unwind().await.is_err() {
        tracing::error!("The maintenance step \"{name}\" stopped with an error; the other steps went on");
    }
}

async fn maintain(st: AppState, trash_days: i64, hours: Arc<AtomicU32>) {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    // After a restart, the first tick is an hour later, as it would have been
    if hours.load(Ordering::Relaxed) > 0 {
        tick.tick().await;
    }
    loop {
        tick.tick().await;
        let hours = hours.fetch_add(1, Ordering::Relaxed) + 1;
        if hours.is_multiple_of(24) {
            step("optimize", db::optimize(&st.db)).await;
        }
        // At the start (the first tick is at once), then daily: space the database and the thumbnails no longer use
        if hours % 24 == 1 {
            step("shrink", db::shrink(&st.db, &st.write_lock)).await;
            step("thumbnails", thumbnails::sweep(&st)).await;
        }
        // Once a day: put the usage counters back in step with the node table, should one ever drift
        if hours.is_multiple_of(24) {
            step("usage", async {
                if let Err(e) = tree::recompute_usage(&st).await {
                    tracing::warn!("Couldn't recompute space usage: {}", e.message);
                }
            })
            .await;
        }
        // Changes of many items that stopped halfway, finished rather than left for the next start
        step("unfinished changes", async {
            if let Err(e) = tree::changes::retry_unfinished(&st).await {
                tracing::warn!("Couldn't finish changes that stopped halfway: {}", e.message);
            }
        })
        .await;
        if trash_days > 0 {
            step("trash", async {
                match nodes::purge_expired_trash(&st, trash_days).await {
                    Ok(n) if n > 0 => tracing::info!("Automatically purged {n} expired trash items"),
                    Err(e) => tracing::warn!("Failed to purge the trash: {}", e.message),
                    _ => {}
                }
            })
            .await;
        }
        // Spaces that are almost full and access that ends soon (#64)
        step("notifications", async {
            if let Err(e) = notify::check(&st).await {
                tracing::warn!("Couldn't check for notifications: {}", e.message);
            }
        })
        .await;
        step("versions", async {
            match versions::prune(&st).await {
                Ok(n) if n > 0 => tracing::info!("Removed {n} earlier versions of files that are no longer kept"),
                Err(e) => tracing::warn!("Couldn't remove earlier versions of files: {}", e.message),
                _ => {}
            }
        })
        .await;
        // The history of backup, replica and move jobs
        step("job history", async {
            match backups::runner::trim_histories(&st).await {
                Ok(n) if n > 0 => tracing::info!("Removed {n} finished jobs past the history kept"),
                Err(e) => tracing::warn!("Couldn't remove old finished jobs: {}", e.message),
                _ => {}
            }
        })
        .await;
        step("S3", crate::s3::maintain(&st)).await;
        step("uploads", async {
            if let Err(e) = upload::purge_expired(&st).await {
                tracing::warn!("Failed to clean up expired uploads: {}", e.message);
            }
            if let Err(e) = upload::clean_tmp(&st).await {
                tracing::warn!("Couldn't clean the temporary directory: {}", e.message);
            }
        })
        .await;
        step("logs", async {
            auth::prune_login_failures(&st);
            logs::prune_share_views(&st);
            logs::daily_archive(&st).await;
        })
        .await;
        step("sessions", async {
            let _w = st.write_lock.lock().await;
            // Sessions that expired, or ended for being idle
            let _ = sqlx::query("DELETE FROM sessions WHERE expires_at < ? OR COALESCE(last_used_at, created_at) < ?")
                .bind(util::now())
                .bind(auth::idle_before())
                .execute(&st.db)
                .await;
        })
        .await;
        crate::metrics::beat(&st, "maintenance");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_step_that_panics_lets_the_next_ones_run() {
        let mut ran = false;
        step("broken", async { panic!("a step that always fails") }).await;
        step("next", async { ran = true }).await;
        assert!(ran);
    }
}
