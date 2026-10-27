//! What may not be deleted for good: a space's retention period and legal holds (holds.rs). Every path that removes a
//! node or its content for good asks here, so the answer is the same wherever the deletion comes from.
//!
//! Retention: a file of a space with `retention_days` can't be deleted for good until that many days after its last
//! change. Legal hold: a hold covers a folder or file and what is in it (a hold on a folder covers every item put in
//! it, and what is below it when it is looked at), a whole space, or the files a person owns. Moving an item to the trash
//! is never refused: it stays there, marked, until nothing keeps it.
//!
//! The questions are answered for a batch in one query, going up (a hold on a folder above) and, for a subtree, down
//! (a hold on something inside). Nothing runs when no hold is in force and no space has a retention period.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::SqliteConnection;

use crate::{error::AppResult, util::now};

/// Why an item can't be deleted for good now
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Kept {
    /// The retention period of its space runs until then (the latest, for a folder: of what is in it)
    pub until: Option<i64>,
    /// The reason of a hold on it or on something it holds, when there is one
    pub hold: Option<String>,
}

impl Kept {
    fn absorb(&mut self, reason: Option<String>, until: Option<i64>) {
        if self.hold.is_none() {
            self.hold = reason;
        }
        self.until = self.until.max(until);
    }
}

/// Whether anything can keep an item at all: a hold in force, or a space with a retention period. The rest of this
/// module costs nothing when it is not.
pub async fn in_force(conn: &mut SqliteConnection) -> AppResult<bool> {
    let (on,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM holds WHERE released_at IS NULL) OR EXISTS (SELECT 1 FROM drives WHERE retention_days > 0)")
        .fetch_one(conn)
        .await?;
    Ok(on)
}

/// The sources of what keeps an item. `{down}` is what the ids reach: only themselves, or everything below them that is
/// not a trash item of its own (those stay when their folder goes, and are looked at on their own).
fn query(subtree: bool) -> String {
    let down = if subtree { "UNION ALL SELECT d.root, n.id FROM down d JOIN nodes n ON n.parent_id = d.id AND n.trash_root = 0" } else { "" };
    format!(
        "WITH RECURSIVE
         down(root, id) AS (SELECT value, value FROM json_each(?1) {down}),
         up(root, id) AS (
           SELECT value, value FROM json_each(?1)
           UNION ALL
           SELECT u.root, n.parent_id FROM up u JOIN nodes n ON n.id = u.id WHERE n.parent_id IS NOT NULL
         )
         SELECT root, reason, until FROM (
           SELECT d.root AS root, h.reason AS reason, NULL AS until
             FROM down d JOIN holds h ON h.scope = 'node' AND h.node_id = d.id AND h.released_at IS NULL
           UNION ALL
           SELECT u.root, h.reason, NULL
             FROM up u JOIN holds h ON h.scope = 'node' AND h.node_id = u.id AND h.released_at IS NULL
           UNION ALL
           SELECT r.value, h.reason, NULL
             FROM json_each(?1) r JOIN nodes n ON n.id = r.value JOIN holds h ON h.scope = 'space' AND h.drive_id = n.drive_id AND h.released_at IS NULL
           UNION ALL
           SELECT d.root, h.reason, NULL
             FROM down d JOIN nodes n ON n.id = d.id JOIN holds h ON h.scope = 'user' AND h.user_id = n.owner_id AND h.released_at IS NULL
            WHERE n.kind = 'file'
           UNION ALL
           SELECT d.root, NULL, n.updated_at + dr.retention_days * 86400
             FROM down d JOIN nodes n ON n.id = d.id JOIN drives dr ON dr.id = n.drive_id
            WHERE n.kind = 'file' AND dr.retention_days > 0 AND n.updated_at + dr.retention_days * 86400 > ?2
         )"
    )
}

async fn ask(conn: &mut SqliteConnection, ids: &[String], subtree: bool) -> AppResult<HashMap<String, Kept>> {
    let mut kept: HashMap<String, Kept> = HashMap::new();
    if ids.is_empty() || !in_force(conn).await? {
        return Ok(kept);
    }
    let sql = query(subtree);
    let rows: Vec<(String, Option<String>, Option<i64>)> =
        sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(serde_json::to_string(ids).unwrap()).bind(now()).fetch_all(conn).await?;
    for (id, reason, until) in rows {
        kept.entry(id).or_default().absorb(reason, until);
    }
    Ok(kept)
}

/// What keeps each of `ids` from being deleted for good, counting what is in them: the ones missing from the answer
/// are free to go. (A trash item's whole subtree, as it is deleted with it.)
pub async fn subtrees(conn: &mut SqliteConnection, ids: &[String]) -> AppResult<HashMap<String, Kept>> {
    ask(conn, ids, true).await
}

/// The same for the items themselves only, not what is in them (a file's content and versions)
pub async fn nodes(conn: &mut SqliteConnection, ids: &[String]) -> AppResult<HashMap<String, Kept>> {
    ask(conn, ids, false).await
}

/// Splits `ids` (the roots of subtrees about to be deleted for good) into those that may go and those kept, in the
/// order given
pub async fn split(conn: &mut SqliteConnection, ids: Vec<String>) -> AppResult<(Vec<String>, Vec<(String, Kept)>)> {
    let mut kept = subtrees(conn, &ids).await?;
    let (mut free, mut held) = (Vec::new(), Vec::new());
    for id in ids {
        match kept.remove(&id) {
            Some(k) => held.push((id, k)),
            None => free.push(id),
        }
    }
    Ok((free, held))
}

/// Whether a space has anything kept in it: a hold on it or on something in it, a hold on a person who owns files in
/// it, or files inside the retention period. Deleting the space would delete all of it.
pub async fn space_has_kept(conn: &mut SqliteConnection, drive_id: &str) -> AppResult<bool> {
    if !in_force(conn).await? {
        return Ok(false);
    }
    let (kept,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM holds WHERE released_at IS NULL AND scope = 'space' AND drive_id = ?1)
             OR EXISTS (SELECT 1 FROM holds h JOIN nodes n ON n.id = h.node_id WHERE h.released_at IS NULL AND h.scope = 'node' AND n.drive_id = ?1)
             OR EXISTS (SELECT 1 FROM holds h JOIN nodes n ON n.owner_id = h.user_id WHERE h.released_at IS NULL AND h.scope = 'user' AND n.drive_id = ?1 AND n.kind = 'file')
             OR EXISTS (SELECT 1 FROM nodes n JOIN drives d ON d.id = n.drive_id
                         WHERE n.drive_id = ?1 AND n.kind = 'file' AND d.retention_days > 0 AND n.updated_at + d.retention_days * 86400 > ?2)",
    )
    .bind(drive_id)
    .bind(now())
    .fetch_one(conn)
    .await?;
    Ok(kept)
}

/// Whether an account can't be deleted: a hold on the files they own, or something kept in their personal space
pub async fn person_kept(conn: &mut SqliteConnection, user_id: i64) -> AppResult<bool> {
    if !in_force(conn).await? {
        return Ok(false);
    }
    let (held,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM holds WHERE released_at IS NULL AND scope = 'user' AND user_id = ?)")
        .bind(user_id)
        .fetch_one(&mut *conn)
        .await?;
    if held {
        return Ok(true);
    }
    let personal: Option<(String,)> =
        sqlx::query_as("SELECT id FROM drives WHERE kind = 'personal' AND owner_id = ?").bind(user_id).fetch_optional(&mut *conn).await?;
    match personal {
        Some((drive,)) => space_has_kept(conn, &drive).await,
        None => Ok(false),
    }
}

/// Moving `node` into the folder `dest`: refused when it would take what is kept out of reach of what keeps it. To
/// another space (a hold on its space, and its retention period, stay behind); or, in the same space, out of a folder
/// that a hold covers (to move a file out and delete it would be a way round the hold).
pub async fn check_move(conn: &mut SqliteConnection, node_id: &str, dest_id: &str, other_space: bool) -> AppResult<()> {
    if !in_force(conn).await? {
        return Ok(());
    }
    if other_space {
        if subtrees(conn, &[node_id.to_string()]).await?.contains_key(node_id) {
            return Err(refusal(MOVE_KEPT));
        }
        return Ok(());
    }
    let (leaves,): (bool,) = sqlx::query_as(
        "WITH RECURSIVE
           above(id) AS (SELECT parent_id FROM nodes WHERE id = ?1 AND parent_id IS NOT NULL
                         UNION ALL SELECT n.parent_id FROM above a JOIN nodes n ON n.id = a.id WHERE n.parent_id IS NOT NULL),
           there(id) AS (SELECT ?2 UNION ALL SELECT n.parent_id FROM there t JOIN nodes n ON n.id = t.id WHERE n.parent_id IS NOT NULL)
         SELECT EXISTS (SELECT 1 FROM holds h WHERE h.scope = 'node' AND h.released_at IS NULL
                          AND h.node_id IN (SELECT id FROM above) AND h.node_id NOT IN (SELECT id FROM there))",
    )
    .bind(node_id)
    .bind(dest_id)
    .fetch_one(conn)
    .await?;
    if leaves {
        return Err(refusal(MOVE_OUT_OF_HELD));
    }
    Ok(())
}

pub const MOVE_KEPT: &str = "This item is kept by a legal hold or a retention period, so it can't be moved to another space";
pub const MOVE_OUT_OF_HELD: &str = "This item is under a legal hold on the folder it is in, so it can't be moved out of that folder";

/// The refusal when something is kept and the change would delete it (or, moving it to another space, lose what keeps it)
pub fn refusal(what: &'static str) -> crate::error::AppError {
    crate::error::AppError::conflict(what).with_code("kept")
}

#[cfg(test)]
mod tests;
