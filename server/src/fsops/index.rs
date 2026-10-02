//! The index: free names, and recording items found or made on disk

use super::*;

/// A name that neither the index nor the disk has in `parent` yet: `name`, else "name (1)", "name (2)"…
pub async fn free_name(conn: &mut SqliteConnection, parent: &Node, name: &str, is_folder: bool) -> AppResult<String> {
    let mut n = 0u32;
    while n < 10_000 {
        // The first names the index doesn't have, then which of them the disk has
        let mut candidates = Vec::new();
        while n < 10_000 && candidates.len() < 20 {
            let candidate = if n == 0 { name.to_string() } else { numbered_name(name, n, is_folder) };
            if !tree::name_taken(conn, &parent.id, &candidate).await? {
                candidates.push(candidate);
            }
            n += 1;
        }
        let dir = parent.clone();
        let free = on_disk(parent.drive(), disk_wait(), move || {
            let dir = abs(&dir)?.dir().map_err(gone_or_disk_error)?;
            Ok(candidates.into_iter().find(|c| !dir.join(c).is_ok_and(|p| std::fs::symlink_metadata(p.as_path()).is_ok())))
        })
        .await?;
        if let Some(free) = free {
            return Ok(free);
        }
    }
    Err(AppError::conflict("Too many items with the same name"))
}

/// Where an item goes in the index: its folder and space, its name, and its path in the space's folder
pub(super) struct At<'a> {
    pub parent: &'a str,
    pub drive: &'a str,
    pub name: &'a str,
    pub rel: &'a str,
}

/// Adds an item that is now on disk at `at.rel` to the index
pub(super) async fn insert_at(conn: &mut SqliteConnection, id: &str, owner: i64, at: At<'_>, s: &Stat) -> AppResult<()> {
    let At { parent: parent_id, drive: drive_id, name, rel } = at;
    let ts = now();
    sqlx::query(
        "INSERT INTO nodes (id, owner_id, parent_id, kind, name, size, mime, drive_id, created_at, updated_at,
                            fs_path, fs_dev, fs_ino, fs_size, fs_mtime_ns, fs_birth_ns)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(owner)
    .bind(parent_id)
    .bind(if s.is_dir { "folder" } else { "file" })
    .bind(name)
    .bind(s.size)
    .bind(if s.is_dir { String::new() } else { guess_mime(name) })
    .bind(drive_id)
    .bind(ts)
    .bind(ts)
    .bind(rel)
    .bind(s.dev)
    .bind(s.ino)
    .bind(s.size)
    .bind(s.mtime_ns)
    .bind(s.birth_ns)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn insert(conn: &mut SqliteConnection, id: &str, owner: i64, parent: &Node, name: &str, rel: &str, s: &Stat) -> AppResult<()> {
    insert_at(conn, id, owner, At { parent: &parent.id, drive: parent.drive(), name, rel }, s).await
}

/// Records where an item is on disk now
pub(super) async fn record(conn: &mut SqliteConnection, id: &str, drive_id: &str, rel: &str, s: &Stat) -> AppResult<()> {
    sqlx::query(
        "UPDATE nodes SET drive_id = ?, fs_path = ?, fs_dev = ?, fs_ino = ?, fs_size = ?, fs_mtime_ns = ?, fs_birth_ns = ?,
                          size = CASE WHEN kind = 'file' THEN ? ELSE size END
         WHERE id = ?",
    )
    .bind(drive_id)
    .bind(rel)
    .bind(s.dev)
    .bind(s.ino)
    .bind(s.size)
    .bind(s.mtime_ns)
    .bind(s.birth_ns)
    .bind(s.size)
    .bind(id)
    .execute(conn)
    .await?;
    Ok(())
}
