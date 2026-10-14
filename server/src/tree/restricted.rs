//! Restricted-folder visibility, including delegated scopes and recursive operations.

use sqlx::SqliteConnection;

use super::{Node, Role, get_drive};
use crate::{
    auth::User,
    error::{AppError, AppResult},
    util::now,
};

/// Whether a user is an owner of the space, rather than the uploader of an item.
pub async fn space_owner(conn: &mut SqliteConnection, user: &User, node: &Node) -> AppResult<bool> {
    let current = crate::auth::current_user(conn, user).await?;
    let user = &current;
    let Some(drive) = get_drive(conn, node.drive()).await? else { return Ok(false) };
    if user.is_admin() && drive.kind != super::SpaceKind::Personal {
        return Ok(true);
    }
    let sql = format!("SELECT EXISTS(SELECT 1 FROM grants g WHERE g.node_id = ?1 AND g.role = 'owner' AND {})", super::permissions::principal_match(2, 3));
    let (owner,): (bool,) = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&drive.root_id).bind(user.id).bind(now()).fetch_one(conn).await?;
    Ok(owner)
}

/// An ancestor publication must not delegate a restricted folder, even when its creator owns it.
/// Publishing the restricted folder itself is an explicit choice and may reach its unrestricted children.
pub async fn within_publication(conn: &mut SqliteConnection, root: &str, id: &str) -> AppResult<bool> {
    let (inside,): (bool,) = sqlx::query_as(
        "WITH RECURSIVE up(id, parent_id, stopped) AS (
           SELECT id, parent_id, restricted FROM nodes WHERE id = ?1
           UNION ALL SELECT n.id, n.parent_id, n.restricted FROM nodes n JOIN up ON n.id = up.parent_id
           WHERE up.id <> ?2 AND NOT up.stopped
         ) SELECT EXISTS(SELECT 1 FROM up WHERE id = ?2)",
    )
    .bind(id)
    .bind(root)
    .fetch_one(conn)
    .await?;
    Ok(inside)
}

/// A SQL predicate for visibility on a node expression. Query values here are internal integer identities, never
/// request text. Lists apply it before paging/counting, so hidden items cannot be inferred from offsets or totals.
pub fn visible_sql(user: &User, node: &str) -> String {
    visible_to_sql(user.id, node)
}

pub fn visible_to_sql(id: i64, node: &str) -> String {
    let at = now();
    let principal = super::permissions::principal_sql(&id.to_string(), &at.to_string());
    format!(
        "EXISTS(WITH RECURSIVE visible_up(id, parent_id, restricted, inherited) AS (
           SELECT id, parent_id, restricted, 1 FROM nodes WHERE id = {node}
           UNION ALL SELECT p.id, p.parent_id, p.restricted, visible_up.inherited AND NOT visible_up.restricted
           FROM nodes p JOIN visible_up ON p.id = visible_up.parent_id
         ) SELECT 1 FROM visible_up JOIN nodes vn ON vn.id = visible_up.id JOIN drives vd ON vd.id = vn.drive_id
           LEFT JOIN grants g ON g.node_id = visible_up.id AND {principal}
           WHERE vd.disabled = 0 AND NOT (vd.kind = 'personal' AND vd.owner_id = {id} AND EXISTS(SELECT 1 FROM users WHERE id = {id} AND guest))
             AND ((g.id IS NOT NULL AND (visible_up.inherited OR (visible_up.parent_id IS NULL AND g.role = 'owner')))
             OR (vd.kind = 'company' AND EXISTS(SELECT 1 FROM users WHERE id = {id} AND role = 'admin' AND NOT guest AND disabled = 0))
             OR (vd.kind <> 'personal' AND EXISTS(SELECT 1 FROM users WHERE id = {id} AND role = 'admin' AND NOT guest AND disabled = 0)
                 AND EXISTS(SELECT 1 FROM visible_up WHERE restricted))))"
    )
}

/// Saved names retain their privacy boundary even after their original item has gone.
pub fn scopes_visible_sql(user: i64, scopes: &str) -> String {
    let visible = visible_to_sql(user, "json_extract(boundary.value, '$.id')");
    let at = now();
    let principal = super::permissions::principal_sql(&user.to_string(), &at.to_string());
    format!(
        "NOT EXISTS(SELECT 1 FROM json_each({scopes}) boundary WHERE NOT (({visible}) OR (
      NOT EXISTS(SELECT 1 FROM nodes WHERE id = json_extract(boundary.value, '$.id')) AND EXISTS(
        SELECT 1 FROM drives d LEFT JOIN grants g ON g.node_id = d.root_id
          AND g.role = 'owner' AND {principal}
        WHERE d.id = json_extract(boundary.value, '$.drive') AND d.disabled = 0 AND (g.id IS NOT NULL
          OR (d.kind <> 'personal' AND EXISTS(SELECT 1 FROM users WHERE id = {user} AND role = 'admin' AND NOT guest AND disabled = 0)))))))"
    )
}

pub async fn saved_scopes(conn: &mut SqliteConnection, id: Option<&str>) -> AppResult<String> {
    let Some(id) = id else { return Ok("[]".into()) };
    let rows: Vec<(String, String)> = sqlx::query_as(
        "WITH RECURSIVE up(id,parent_id,restricted,drive_id) AS (
      SELECT id,parent_id,restricted,drive_id FROM nodes WHERE id = ?1 UNION ALL
      SELECT n.id,n.parent_id,n.restricted,n.drive_id FROM nodes n JOIN up ON n.id = up.parent_id)
      SELECT id,drive_id FROM up WHERE restricted ORDER BY id",
    )
    .bind(id)
    .fetch_all(conn)
    .await?;
    Ok(serde_json::Value::Array(rows.into_iter().map(|(id, drive)| serde_json::json!({"id": id, "drive": drive})).collect()).to_string())
}

/// A directly shared descendant can be reached without revealing inaccessible folders above it.
pub async fn visible_path_start(conn: &mut SqliteConnection, user: &User, path: &[super::Crumb]) -> AppResult<usize> {
    if path.is_empty() {
        return Ok(0);
    }
    let sql = format!("SELECT n.id FROM nodes n WHERE n.id IN (SELECT value FROM json_each(?1)) AND NOT ({})", visible_sql(user, "n.id"));
    let ids = serde_json::to_string(&path.iter().map(|c| &c.id).collect::<Vec<_>>()).map_err(AppError::internal)?;
    let hidden: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(ids).fetch_all(conn).await?;
    Ok(path.iter().rposition(|c| hidden.iter().any(|(id,)| *id == c.id)).map_or(0, |index| index + 1))
}

/// A restriction also protects historical names, including older rows whose child was already deleted. Missing
/// notification targets have no surviving space identity: conservatively protect them rather than guess a scope.
pub async fn protect_saved(conn: &mut SqliteConnection, node: &Node) -> AppResult<()> {
    let scope = serde_json::json!({"id": node.id, "drive": node.drive()}).to_string();
    for (table, space) in [("activity", "AND drive_id = ?3"), ("share_access", ""), ("notifications", "")] {
        let sql = format!(
            "WITH RECURSIVE sub(id) AS (SELECT ?1 UNION ALL SELECT n.id FROM nodes n JOIN sub ON n.parent_id = sub.id)
          UPDATE {table} SET restricted_scopes = json_insert(restricted_scopes, '$[#]', json(?2))
          WHERE node_id IS NOT NULL {space} AND (node_id IN (SELECT id FROM sub) OR NOT EXISTS(SELECT 1 FROM nodes WHERE id = node_id))
          AND NOT EXISTS(SELECT 1 FROM json_each(restricted_scopes) WHERE json_extract(value, '$.id') = ?1)"
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(&node.id).bind(&scope);
        if !space.is_empty() {
            query = query.bind(node.drive());
        }
        query.execute(&mut *conn).await?;
    }
    Ok(())
}

// Carry the flag while walking the subtree. A final join back to nodes lets SQLite scan unrelated rows by id.
const RESTRICTED_SUBTREE_SQL: &str = "WITH RECURSIVE sub(id, restricted) AS (
    SELECT id, restricted FROM nodes WHERE id = ?1
    UNION ALL SELECT n.id, n.restricted FROM nodes n JOIN sub ON n.parent_id = sub.id
  ) SELECT id FROM sub WHERE restricted AND id > ?2 ORDER BY id LIMIT 128";

/// Moving or deleting a parent must never carry along descendants the caller cannot act on. Refusal is generic:
/// neither the name nor even the number of restricted descendants is disclosed.
pub async fn require_subtree(conn: &mut SqliteConnection, user: &User, node: &Node, need: super::Need) -> AppResult<()> {
    let mut after = String::new();
    loop {
        let ids: Vec<(String,)> = sqlx::query_as(RESTRICTED_SUBTREE_SQL).bind(&node.id).bind(&after).fetch_all(&mut *conn).await?;
        if ids.is_empty() {
            return Ok(());
        }
        for (id,) in &ids {
            let item = super::get_node(conn, id).await?.ok_or_else(|| AppError::conflict("The folder changed while checking access"))?;
            let role = super::role_on(conn, user, &item).await?.ok_or_else(|| AppError::forbidden("You don't have access to everything in this folder"))?;
            // Contributor ownership is checked once by the root operation, including actual folder-space entries.
            if !(role == Role::Contributor && super::is_own(user, &item)) {
                super::allows(user, role, need).map_err(|_| AppError::forbidden("You don't have permission to change everything in this folder"))?;
            }
        }
        after = ids.last().unwrap().0.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{testutil, tree::Need};

    #[tokio::test]
    async fn checking_an_empty_subtree_does_not_scan_unrelated_nodes() {
        let env = testutil::env().await;
        let owner = env.user("owner", true).await;
        let empty = env.folder(&owner, owner.root(), "Empty").await;
        sqlx::query(
            "WITH RECURSIVE items(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM items WHERE n < 5000)
             INSERT INTO nodes (id, owner_id, parent_id, kind, name, drive_id, created_at, updated_at, restricted)
             SELECT printf('unrelated-%05d', n), ?1, ?2, 'folder', printf('Unrelated %d', n),
                    (SELECT drive_id FROM nodes WHERE id = ?2), ?3, ?3, 1 FROM items",
        )
        .bind(owner.id)
        .bind(owner.root())
        .bind(now())
        .execute(&env.st.db)
        .await
        .unwrap();
        let explain = format!("EXPLAIN QUERY PLAN {RESTRICTED_SUBTREE_SQL}");
        let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(explain.as_str())).bind(&empty).bind("").fetch_all(&env.st.db).await.unwrap();
        let (version,): (String,) = sqlx::query_as("SELECT sqlite_version()").fetch_one(&env.st.db).await.unwrap();
        println!("SQLite {version}: {plan:?}");
        assert!(
            plan.iter().all(|(_, _, _, detail)| !detail.starts_with("SCAN n ") && !(detail.starts_with("SEARCH n ") && detail.contains("(id>?)"))),
            "An empty subtree must not search unrelated node rows: {plan:?}"
        );
        let mut conn = env.st.db.acquire().await.unwrap();
        let node = super::super::get_node(&mut conn, &empty).await.unwrap().unwrap();
        require_subtree(&mut conn, &owner, &node, Need::Delete).await.unwrap();
    }

    #[tokio::test]
    async fn subtree_checks_keep_restricted_descendants_after_the_first_page() {
        let env = testutil::env().await;
        let owner = env.user("owner", true).await;
        let editor = env.user("editor", true).await;
        let parent = env.folder(&owner, owner.root(), "Shared").await;
        env.grant(&parent, &editor, "editor").await;
        sqlx::query(
            "WITH RECURSIVE items(n) AS (SELECT 0 UNION ALL SELECT n + 1 FROM items WHERE n < 128)
             INSERT INTO nodes (id, owner_id, parent_id, kind, name, drive_id, created_at, updated_at, restricted)
             SELECT printf('restricted-%03d', n), ?1, ?2, 'folder', printf('Restricted %d', n),
                    (SELECT drive_id FROM nodes WHERE id = ?2), ?3, ?3, 1 FROM items",
        )
        .bind(owner.id)
        .bind(&parent)
        .bind(now())
        .execute(&env.st.db)
        .await
        .unwrap();
        for i in 0..129 {
            env.grant(&format!("restricted-{i:03}"), &editor, if i == 128 { "viewer" } else { "editor" }).await;
        }
        let mut conn = env.st.db.acquire().await.unwrap();
        let node = super::super::get_node(&mut conn, &parent).await.unwrap().unwrap();
        require_subtree(&mut conn, &owner, &node, Need::Delete).await.unwrap();
        let error = require_subtree(&mut conn, &editor, &node, Need::Delete).await.unwrap_err();
        assert_eq!(error.status, axum::http::StatusCode::FORBIDDEN);
        assert!(!error.message.contains("Restricted"));
        sqlx::query("UPDATE grants SET role = 'editor' WHERE node_id = 'restricted-128'").execute(&mut *conn).await.unwrap();
        require_subtree(&mut conn, &editor, &node, Need::Delete).await.unwrap();
    }
}
