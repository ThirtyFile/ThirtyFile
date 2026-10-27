//! What a scrape shows that costs something to find out: counts from the database, the size of its files, the disks,
//! the usage page's capacity samples. A task takes them every fifteen seconds and keeps them in memory, so a scrape
//! (which a monitoring system makes as often as it likes) only reads memory: it takes no write lock and reads no disk.

use std::{collections::BTreeMap, time::Duration};

use super::Memory;
use crate::{error::AppResult, state::AppState, util::now};

/// How often the figures are looked at again
const EVERY: Duration = Duration::from_secs(15);

/// A storage location's capacity, as the usage page last sampled it
#[derive(Debug, Clone, Default)]
pub struct Location {
    pub name: String,
    pub online: bool,
    /// What the location holds of the files in spaces: live, in the trash, earlier versions
    pub live_bytes: i64,
    pub trash_bytes: i64,
    pub version_bytes: i64,
    pub disk_free: Option<i64>,
    pub disk_total: Option<i64>,
}

/// A disk the server's folders are on
#[derive(Debug, Clone)]
pub struct Volume {
    pub volume: &'static str,
    pub free: i64,
    pub total: i64,
}

/// What was found at `taken_at`
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub taken_at: i64,
    pub sessions: i64,
    pub accounts_active: i64,
    pub accounts_disabled: i64,
    pub database_bytes: i64,
    pub wal_bytes: i64,
    pub volumes: Vec<Volume>,
    pub locations: Vec<Location>,
    /// Location id → name, for the operation counters, which know only ids
    pub location_names: std::collections::HashMap<String, String>,
    /// Alerts firing now, by kind
    pub alerts: BTreeMap<String, i64>,
    /// Policy name → seconds since its last complete snapshot (None: it has made none)
    pub backups: Vec<(String, Option<i64>)>,
    /// Policy name → seconds since its slowest target was last up to date (None: never)
    pub replicas: Vec<(String, Option<i64>)>,
    pub search_queued: i64,
    pub search_failed: i64,
    /// Administrator alert messages waiting to be sent, and given up on
    pub deliveries_pending: i64,
    pub deliveries_failed: i64,
    /// Seconds until the FTPS certificate ends
    pub certificate_seconds: Option<i64>,
    pub memory_bytes: Option<i64>,
    pub open_files: Option<i64>,
    pub threads: Option<i64>,
}

/// Starts the task that keeps the snapshot current
pub fn spawn(st: AppState) {
    crate::util::supervise("metrics", move |_| run(st.clone()));
}

async fn run(st: AppState) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        // Only while the page is on: nothing is looked at for a server nobody scrapes
        if !st.part::<Memory>().settings.read().enabled {
            continue;
        }
        match take(&st).await {
            Ok(snapshot) => *st.part::<Memory>().snapshot.write() = snapshot,
            Err(e) => tracing::warn!("Couldn't collect the monitoring figures: {}", e.message),
        }
    }
}

/// Looks at everything once
pub async fn take(st: &AppState) -> AppResult<Snapshot> {
    let t = now();
    let (sessions,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE expires_at > ?").bind(t).fetch_one(&st.db).await?;
    let (accounts_active, accounts_disabled): (i64, i64) =
        sqlx::query_as("SELECT COALESCE(SUM(disabled = 0), 0), COALESCE(SUM(disabled <> 0), 0) FROM users").fetch_one(&st.db).await?;
    let (pages, page_size): (i64, i64) = sqlx::query_as("SELECT page_count, page_size FROM pragma_page_count(), pragma_page_size()").fetch_one(&st.db).await?;
    let wal_bytes = std::fs::metadata(st.data_dir.join("drive.db-wal")).map_or(0, |m| i64::try_from(m.len()).unwrap_or(i64::MAX));

    let mut volumes = Vec::new();
    for (volume, path) in [("data", st.data_dir.clone()), ("storage", st.storage_dir.clone())] {
        if let Some((free, total, _)) = crate::usage::sample::disk(path).await {
            volumes.push(Volume { volume, free, total });
        }
    }

    // The usage page's own figures: the capacity samples and what is firing
    let overview = crate::usage::api::build_overview(st, t).await?;
    let locations = overview
        .locations
        .iter()
        .map(|l| {
            let c = l.capacity.as_ref().map(|s| &s.capacity);
            Location {
                name: l.name.clone(),
                online: l.online,
                live_bytes: c.map_or(0, |c| c.live_bytes),
                trash_bytes: c.map_or(0, |c| c.trash_bytes),
                version_bytes: c.map_or(0, |c| c.version_bytes),
                disk_free: c.and_then(|c| c.disk_free),
                disk_total: c.and_then(|c| c.disk_total),
            }
        })
        .collect();
    let location_names = overview.locations.iter().map(|l| (l.id.clone(), l.name.clone())).collect();
    let mut alerts: BTreeMap<String, i64> = BTreeMap::new();
    let firing: Vec<(String,)> = sqlx::query_as("SELECT kind FROM alert_conditions WHERE active = 1").fetch_all(&st.db).await.unwrap_or_default();
    for (kind,) in firing {
        *alerts.entry(kind).or_default() += 1;
    }

    let backups: Vec<(String, Option<i64>)> = sqlx::query_as::<_, (String, Option<i64>)>(
        "SELECT s.name, (SELECT MAX(n.completed_at) FROM backup_snapshots n WHERE n.set_id = s.id AND n.state = 'complete')
         FROM backup_policies p JOIN backup_sets s ON s.id = p.set_id WHERE p.enabled = 1 ORDER BY s.name",
    )
    .fetch_all(&st.db)
    .await?
    .into_iter()
    .map(|(name, last)| (name, last.map(|at| (t - at).max(0))))
    .collect();
    let replicas: Vec<(String, Option<i64>)> = sqlx::query_as::<_, (String, Option<i64>, i64)>(
        "SELECT p.name, MIN(x.synced_at), SUM(x.synced_at IS NULL)
         FROM replica_policies p JOIN replica_targets x ON x.policy_id = p.id WHERE p.enabled = 1 AND x.state = 'active' GROUP BY p.id ORDER BY p.name",
    )
    .fetch_all(&st.db)
    .await?
    .into_iter()
    // A target that never finished a sync makes the policy "never"
    .map(|(name, oldest, never)| (name, if never > 0 { None } else { oldest.map(|at| (t - at).max(0)) }))
    .collect();

    let (search_queued,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM content_queue").fetch_one(&st.db).await?;
    let (search_failed,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM content_index WHERE state = 'failed'").fetch_one(&st.db).await?;
    let (deliveries_pending, deliveries_failed): (i64, i64) =
        sqlx::query_as("SELECT COALESCE(SUM(done_at IS NULL AND failed = 0), 0), COALESCE(SUM(failed), 0) FROM alert_deliveries").fetch_one(&st.db).await?;
    let certificate_seconds = st.part::<crate::ftp::Memory>().status.read().cert_expires_at.map(|ends| ends - t);
    let process = process();

    Ok(Snapshot {
        taken_at: t,
        sessions,
        accounts_active,
        accounts_disabled,
        database_bytes: pages.saturating_mul(page_size),
        wal_bytes,
        volumes,
        locations,
        location_names,
        alerts,
        backups,
        replicas,
        search_queued,
        search_failed,
        deliveries_pending,
        deliveries_failed,
        certificate_seconds,
        memory_bytes: process.memory,
        open_files: process.files,
        threads: process.threads,
    })
}

#[derive(Default)]
struct Process {
    memory: Option<i64>,
    files: Option<i64>,
    threads: Option<i64>,
}

/// The server's own memory, open files and threads, where the system says (Linux: /proc)
fn process() -> Process {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let field = |name: &str| status.lines().find_map(|l| l.strip_prefix(name)?.trim().split_whitespace().next()?.parse::<i64>().ok());
        Process {
            memory: field("VmRSS:").map(|kb| kb.saturating_mul(1024)),
            files: std::fs::read_dir("/proc/self/fd").ok().map(|d| i64::try_from(d.count()).unwrap_or(i64::MAX)),
            threads: field("Threads:"),
        }
    }
    #[cfg(not(target_os = "linux"))]
    Process::default()
}
