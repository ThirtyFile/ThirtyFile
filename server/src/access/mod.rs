//! What every file protocol needs, whatever it speaks: WebDAV now, then SFTP, S3 buckets and FTP receive folders
//! (#553). A protocol turns its requests into these calls and their answers into its own; the calls go through the
//! same code as the web, so permissions, read-only spaces, folder spaces, quotas, the upload size limit, name rules,
//! versions, the trash and the activity log are the same everywhere.
//!
//! - `paths`: the folder a new item goes into, and the item a path names (the tree is `crate::paths`: the user's
//!   spaces and Shared with me, as the address bar shows them)
//! - `write`: receiving a file within the limits, and storing it as a new file or new content
//! - `changes`: new folders, renames, moves, copies, the trash, through the web's handlers
//! - `space_room`: what a space has used and has left
//!
//! Errors are `AppError`s with HTTP statuses; a protocol that isn't HTTP maps them to its own codes.

mod changes;
mod paths;
mod read;
mod write;

pub use changes::*;
pub use paths::*;
pub use read::*;
pub use write::*;

use std::collections::HashMap;

use sqlx::SqliteConnection;

use crate::{error::AppResult, tree};

/// (used, available) bytes of a space with a quota, remembered per space for one answer (`cache`); None without one
pub async fn space_room(conn: &mut SqliteConnection, cache: &mut HashMap<String, Option<(i64, i64)>>, drive_id: &str) -> AppResult<Option<(i64, i64)>> {
    if let Some(q) = cache.get(drive_id) {
        return Ok(*q);
    }
    let q = match tree::get_drive(conn, drive_id).await? {
        Some(d) => {
            let limit = tree::drive_quota(conn, &d).await?;
            (limit > 0).then(|| (d.used_bytes, (limit - d.used_bytes).max(0)))
        }
        None => None,
    };
    cache.insert(drive_id.to_string(), q);
    Ok(q)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use axum::body::Bytes;

    fn body(text: &'static str) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Unpin {
        futures_util::stream::iter([Ok(Bytes::from_static(text.as_bytes()))])
    }

    #[tokio::test]
    async fn a_file_is_written_then_replaced_keeping_a_version_and_refused_beyond_the_quota() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let segs = |name: &str| vec![crate::paths::MY_FILES.to_string(), name.to_string()];
        let (parent, name) = parent_and_name(&mut env.st.db.acquire().await.unwrap(), &amy, &segs("notes.txt")).await.unwrap();
        let up = upload_to(&env.st, &amy, &parent, name.clone()).await.unwrap();
        assert!(up.existing.is_none());
        assert!(write(&env.st, &amy, up, body("first"), Some(5)).await.unwrap(), "created");
        let up = upload_to(&env.st, &amy, &parent, name.clone()).await.unwrap();
        assert!(up.existing.is_some());
        assert!(!write(&env.st, &amy, up, body("second"), None).await.unwrap(), "replaced");
        let (versions,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM node_versions v JOIN nodes n ON n.id = v.node_id WHERE n.name = 'notes.txt'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(versions, 1, "the first content is kept as an earlier version");

        // Beyond the space's size: refused before receiving when the size is known, and as soon as it is passed when not
        sqlx::query("UPDATE users SET quota_bytes = 8 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        let up = upload_to(&env.st, &amy, &parent, "big.txt".into()).await.unwrap();
        assert!(write(&env.st, &amy, up, body("far too long"), Some(12)).await.is_err());
        let up = upload_to(&env.st, &amy, &parent, "big.txt".into()).await.unwrap();
        assert!(write(&env.st, &amy, up, body("far too long"), None).await.is_err());
        assert!(crate::paths::child_named(&mut env.st.db.acquire().await.unwrap(), &parent.id, "big.txt").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn new_items_go_only_inside_a_space_and_names_follow_the_rules() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let mut c = env.st.db.acquire().await.unwrap();
        let path = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(parent_and_name(&mut c, &amy, &path(&[])).await.is_err(), "the top isn't a folder to add to");
        assert!(parent_and_name(&mut c, &amy, &path(&["New space"])).await.is_err());
        assert!(parent_and_name(&mut c, &amy, &path(&[crate::paths::SHARED, "x"])).await.is_err());
        assert!(parent_and_name(&mut c, &amy, &path(&[crate::paths::MY_FILES, "missing", "x"])).await.is_err());
        assert!(parent_and_name(&mut c, &amy, &path(&[crate::paths::MY_FILES, " padded "])).await.is_err());
        let (folder, name) = parent_and_name(&mut c, &amy, &path(&[crate::paths::MY_FILES, "fine.txt"])).await.unwrap();
        assert_eq!((folder.id.as_str(), name.as_str()), (amy.root(), "fine.txt"));
        drop(c);
        assert!(item(&env.st, &amy, &path(&[crate::paths::MY_FILES])).await.is_ok(), "a space's root is an item");
        assert!(item(&env.st, &amy, &path(&[crate::paths::SHARED])).await.is_err());
    }
}
