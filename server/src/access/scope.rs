//! Relative paths rooted at a stable folder identity. Adapters never interpret `..`, empty components or aliases.

use sqlx::SqliteConnection;
use crate::{auth::User, error::{AppError, AppResult}, paths::child_named, state::AppState, tree::{self, Node, Need}, util::validate_name};

pub fn relative_parts(key: &str) -> AppResult<Vec<String>> {
    if key.is_empty() || key.len() > 1024 || key.starts_with('/') || key.contains('\\') { return Err(AppError::bad_request("Invalid object key")); }
    let key = key.strip_suffix('/').unwrap_or(key);
    key.split('/').map(|part| {
        let name = validate_name(part)?;
        if name != part || part.is_empty() || part == "." || part == ".." { return Err(AppError::bad_request("Invalid object key")); }
        Ok(name)
    }).collect()
}

/// Checked inside write transactions too: a folder moved outside the published root while an upload arrived is refused.
pub async fn inside(conn: &mut SqliteConnection, root: &str, node: &str) -> AppResult<bool> {
    let (found,): (bool,) = sqlx::query_as("WITH RECURSIVE up(id, parent_id) AS (SELECT id, parent_id FROM nodes WHERE id = ?1 UNION ALL SELECT n.id,n.parent_id FROM nodes n JOIN up ON n.id = up.parent_id) SELECT EXISTS(SELECT 1 FROM up WHERE id = ?2)").bind(node).bind(root).fetch_one(conn).await?;
    Ok(found)
}

pub async fn rooted_item(st: &AppState, user: &User, root: &str, key: &str, need: Need) -> AppResult<Node> {
    let parts = relative_parts(key)?;
    let mut node = tree::folder_for(&mut *st.db.acquire().await?, user, root, Need::Read).await?;
    for part in parts {
        if !node.is_folder() { return Err(AppError::not_found("Object not found")); }
        if node.in_folder_space() { crate::folders::sync_folder(st, &node).await; }
        let mut conn = st.db.acquire().await?;
        node = child_named(&mut conn, &node.id, &part).await?.ok_or_else(|| AppError::not_found("Object not found"))?;
        if node.name != part { return Err(AppError::conflict("Object keys that differ only in case conflict in this space")); }
        node = tree::node_for(&mut conn, user, &node.id, need).await?;
        if !inside(&mut conn, root, &node.id).await? { return Err(AppError::forbidden("Object is outside the bucket")); }
    }
    if node.is_folder() != key.ends_with('/') { return Err(AppError::not_found("Object not found")); }
    Ok(node)
}

pub async fn rooted_parent(st: &AppState, user: &User, root: &str, key: &str) -> AppResult<(Node, String)> {
    let mut parts = relative_parts(key)?;
    let name = parts.pop().ok_or_else(|| AppError::bad_request("Invalid object key"))?;
    let mut folder = tree::folder_for(&mut *st.db.acquire().await?, user, root, Need::Add).await?;
    for part in parts {
        if folder.in_folder_space() { crate::folders::sync_folder(st, &folder).await; }
        let child = child_named(&mut st.db.acquire().await?, &folder.id, &part).await?;
        if child.is_none() {
            // Under the common write lock, recheck the scope before adding anything.
            let _w = st.write_lock.lock().await;
            let mut tx = crate::db::begin_write(&st.db).await?;
            let parent = tree::folder_for(&mut tx, user, &folder.id, Need::Add).await?;
            if !inside(&mut tx, root, &parent.id).await? { return Err(AppError::forbidden("Object is outside the bucket")); }
            if child_named(&mut tx, &parent.id, &part).await?.is_none() && !parent.in_folder_space() {
                crate::content::create_folder(&mut tx, user.id, &parent.id, &part).await?;
                tx.commit().await?;
            } else if parent.in_folder_space() {
                drop(tx); drop(_w);
                super::make_folder(st, user, &parent, &part).await?;
            }
        }
        let mut conn = st.db.acquire().await?;
        let next = child_named(&mut conn, &folder.id, &part).await?.ok_or_else(|| AppError::conflict("The folder changed at the same time"))?;
        if next.name != part { return Err(AppError::conflict("Object keys that differ only in case conflict in this space")); }
        folder = tree::folder_for(&mut conn, user, &next.id, Need::Add).await?;
        if !inside(&mut conn, root, &folder.id).await? { return Err(AppError::forbidden("Object is outside the bucket")); }
    }
    Ok((folder, name))
}
