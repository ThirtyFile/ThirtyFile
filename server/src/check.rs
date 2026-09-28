//! `thirtyfile check [--verify]`: compares the content store with the database, for every storage location.
//!
//! Reports content the database knows that its location doesn't have (or has with another size), files in a location
//! the database doesn't know (listed only, never deleted), and with `--verify`, content whose hash no longer matches.
//! Folder spaces keep their files as they are and aren't part of the content store.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::io::AsyncReadExt;

use crate::storage::Storage;

#[derive(Default)]
pub struct LocationReport {
    pub checked: usize,
    /// Known to the database, missing in storage
    pub missing: Vec<String>,
    /// Stored with another size than the database says: (hash, expected, found)
    pub wrong_size: Vec<(String, u64, u64)>,
    /// In storage, unknown to the database (and not waiting to be deleted)
    pub unknown: Vec<String>,
    /// Read back with another hash (`--verify`)
    pub corrupt: Vec<String>,
    /// The location couldn't be read (offline, or it can't be listed)
    pub error: Option<String>,
}

impl LocationReport {
    pub fn problems(&self) -> usize {
        self.missing.len() + self.wrong_size.len() + self.corrupt.len() + usize::from(self.error.is_some())
    }
}

/// Checks every location; `progress` is told about each location as it starts
pub async fn run(
    db: &SqlitePool,
    storages: &HashMap<String, Arc<dyn Storage>>,
    verify: bool,
    progress: impl Fn(&str, usize),
) -> Result<Vec<(String, LocationReport)>, sqlx::Error> {
    let blobs: Vec<(String, i64, String)> = sqlx::query_as("SELECT hash, size, location_id FROM blobs").fetch_all(db).await?;
    let pending: Vec<(String, String)> = sqlx::query_as("SELECT hash, location_id FROM pending_blob_deletes").fetch_all(db).await?;
    let mut by_location: HashMap<String, Vec<(String, u64)>> = HashMap::new();
    for (hash, size, location) in blobs {
        by_location.entry(location).or_default().push((hash, size.max(0) as u64));
    }
    let mut ids: Vec<&String> = storages.keys().collect();
    ids.sort();
    let mut out = Vec::new();
    for id in ids {
        let storage = &storages[id];
        let known = by_location.remove(id).unwrap_or_default();
        progress(id, known.len());
        let mut report = LocationReport { checked: known.len(), ..Default::default() };
        let listed: Option<HashSet<String>> = match storage.list().await {
            Ok(list) => Some(list.into_iter().collect()),
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => None,
            Err(e) => {
                report.error = Some(e.to_string());
                out.push((id.clone(), report));
                continue;
            }
        };
        for (hash, size) in &known {
            if listed.as_ref().is_some_and(|l| !l.contains(hash)) {
                report.missing.push(hash.clone());
                continue;
            }
            match storage.size(hash).await {
                Ok(None) => report.missing.push(hash.clone()),
                Ok(Some(found)) if found != *size => report.wrong_size.push((hash.clone(), *size, found)),
                Ok(Some(_)) => {
                    if verify && !matches_hash(storage.as_ref(), hash, *size).await.unwrap_or(false) {
                        report.corrupt.push(hash.clone());
                    }
                }
                Err(e) => {
                    report.error = Some(e.to_string());
                    break;
                }
            }
        }
        if let Some(listed) = listed {
            let known: HashSet<&str> = known.iter().map(|(h, _)| h.as_str()).collect();
            let waiting: HashSet<&str> = pending.iter().filter(|(_, l)| l == id).map(|(h, _)| h.as_str()).collect();
            report.unknown = listed.into_iter().filter(|h| !known.contains(h.as_str()) && !waiting.contains(h.as_str())).collect();
            report.unknown.sort();
        }
        out.push((id.clone(), report));
    }
    // Content recorded in a location that no longer exists
    for (id, known) in by_location {
        out.push((id, LocationReport { checked: known.len(), missing: known.into_iter().map(|(h, _)| h).collect(), ..Default::default() }));
    }
    Ok(out)
}

async fn matches_hash(storage: &dyn Storage, hash: &str, size: u64) -> std::io::Result<bool> {
    let mut reader = storage.open(hash, 0, size).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()) == hash)
}

/// The report as text for the terminal; returns the number of problems
pub fn print(reports: &[(String, LocationReport)]) -> usize {
    let mut problems = 0;
    for (id, r) in reports {
        println!("Storage location {id}: {} stored file(s) checked", r.checked);
        if let Some(e) = &r.error {
            println!("  couldn't be read: {e}");
        }
        for h in &r.missing {
            println!("  missing: {h}");
        }
        for (h, expected, found) in &r.wrong_size {
            println!("  wrong size: {h} (expected {expected} bytes, found {found})");
        }
        for h in &r.corrupt {
            println!("  content doesn't match its hash: {h}");
        }
        if !r.unknown.is_empty() {
            println!("  {} file(s) the database doesn't know (left as they are):", r.unknown.len());
            for h in &r.unknown {
                println!("    {h}");
            }
        }
        problems += r.problems();
    }
    println!("{}", if problems == 0 { "No problems found".to_string() } else { format!("{problems} problem(s) found") });
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn check_finds_missing_changed_and_unknown_content() {
        let env = testutil::env().await;
        let local = env.st.storage("local").unwrap();
        let put = |content: &'static str| {
            let (local, dir) = (local.clone(), env.dir.clone());
            async move {
                let hash = hex::encode(Sha256::digest(content.as_bytes()));
                let tmp = dir.join("tmp").join(&hash);
                std::fs::write(&tmp, content).unwrap();
                local.put_file(&hash, &tmp).await.unwrap();
                hash
            }
        };
        let good = put("good content").await;
        let changed = put("will be changed").await;
        let stray = put("nobody knows me").await;
        let gone = hex::encode(Sha256::digest(b"never stored"));
        for (hash, size) in [(&good, 12), (&changed, 15), (&gone, 12)] {
            sqlx::query("INSERT INTO blobs (hash, size, refcount, created_at, location_id) VALUES (?, ?, 1, 0, 'local')")
                .bind(hash)
                .bind(size)
                .execute(&env.st.db)
                .await
                .unwrap();
        }
        // Same size, other bytes: only --verify notices
        let path = env.dir.join("blobs").join(&changed[0..2]).join(&changed[2..4]).join(&changed);
        std::fs::write(&path, "WILL be changed").unwrap();
        let storages = env.st.storages.read().unwrap().clone();
        let reports = run(&env.st.db, &storages, true, |_, _| {}).await.unwrap();
        let (_, r) = reports.iter().find(|(id, _)| id == "local").unwrap();
        assert_eq!(r.missing, [gone]);
        assert_eq!(r.corrupt, [changed]);
        assert_eq!(r.unknown, [stray]);
        assert!(r.wrong_size.is_empty() && r.error.is_none());
        assert_eq!(print(&reports), 2);
    }
}
