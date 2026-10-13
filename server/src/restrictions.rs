//! Owner-controlled inheritance boundaries, serialized with file commits.

use crate::{
    auth::User,
    error::{AppError, AppResult},
    logs,
    state::AppState,
    tree::{self, Node},
};
use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

async fn controlled(conn: &mut sqlx::SqliteConnection, user: &User, id: &str) -> AppResult<Node> {
    let node = tree::get_node(conn, id).await?.ok_or_else(|| AppError::not_found("Item not found"))?;
    if !tree::space_owner(conn, user, &node).await? {
        return Err(AppError::forbidden("Only space owners and administrators can change folder restrictions"));
    }
    if !node.is_folder() || node.parent_id.is_none() || node.trashed_at.is_some() {
        return Err(AppError::bad_request("Only folders outside the trash can be restricted. Space roots cannot be restricted."));
    }
    let (in_trash,): (bool,) = sqlx::query_as(
        "WITH RECURSIVE up(id,parent_id,trash) AS (
      SELECT id,parent_id,trashed_at IS NOT NULL FROM nodes WHERE id = ?1 UNION ALL
      SELECT n.id,n.parent_id,n.trashed_at IS NOT NULL FROM nodes n JOIN up ON n.id = up.parent_id)
      SELECT EXISTS(SELECT 1 FROM up WHERE trash)",
    )
    .bind(&node.id)
    .fetch_one(&mut *conn)
    .await?;
    if in_trash || tree::get_drive(conn, node.drive()).await?.is_none_or(|d| d.disabled) {
        return Err(AppError::not_found("Item not found"));
    }
    Ok(node)
}

#[derive(Serialize, sqlx::FromRow)]
pub struct Inherited {
    principal_type: tree::PrincipalType,
    principal_id: i64,
    name: String,
    role: tree::Role,
}

#[derive(Serialize)]
pub struct Preview {
    restricted: bool,
    inherited: Vec<Inherited>,
}

pub async fn preview(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<Preview>> {
    let mut c = st.db.acquire().await?;
    let node = controlled(&mut c, &user, &id).await?;
    let inherited = sqlx::query_as(
        "WITH RECURSIVE up(id, parent_id, restricted) AS (
           SELECT id, parent_id, restricted FROM nodes WHERE id = ?1
           UNION ALL SELECT n.id,n.parent_id,n.restricted FROM nodes n JOIN up ON n.id = up.parent_id WHERE NOT up.restricted
         ) SELECT g.principal_type, g.principal_id, g.role,
           CASE g.principal_type WHEN 'user' THEN COALESCE(u.username, '(deleted)') WHEN 'group' THEN COALESCE(gr.name, '(deleted)') ELSE 'Everyone' END AS name
         FROM grants g JOIN up ON g.node_id = up.id LEFT JOIN users u ON g.principal_type = 'user' AND u.id = g.principal_id
         LEFT JOIN groups gr ON g.principal_type = 'group' AND gr.id = g.principal_id
         WHERE g.role <> 'owner' AND (g.expires_at IS NULL OR g.expires_at > ?2) ORDER BY name,g.role",
    )
    .bind(&node.parent_id)
    .bind(crate::util::now())
    .fetch_all(&mut *c)
    .await?;
    Ok(Json(Preview { restricted: node.restricted, inherited }))
}

#[derive(Deserialize)]
pub struct Change {
    restricted: bool,
    expected: bool,
    #[serde(default)]
    confirmed: bool,
}

pub async fn change(State(st): State<AppState>, user: User, Path(id): Path<String>, Json(req): Json<Change>) -> AppResult<Json<Value>> {
    if !req.confirmed {
        return Err(AppError::bad_request("Confirm the change to inherited access first"));
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let node = controlled(&mut tx, &user, &id).await?;
    if node.restricted != req.expected {
        return Err(AppError::conflict("Access changed. Reload this dialog before changing it."));
    }
    if node.restricted != req.restricted {
        if req.restricted {
            tree::protect_saved(&mut tx, &node).await?;
        }
        sqlx::query("UPDATE nodes SET restricted = ?, updated_at = ? WHERE id = ?")
            .bind(req.restricted)
            .bind(crate::util::now())
            .bind(&node.id)
            .execute(&mut *tx)
            .await?;
        logs::record_activity(&mut tx, &user, Some(&node), "restriction", if req.restricted { "Restricted access" } else { "Inherited access restored" }).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn restricted_folders_cut_manager_inheritance_and_keep_owner_access() {
        let env = testutil::env().await;
        let owner = env.user("owner", true).await;
        let manager = env.user("manager", true).await;
        let department = env.folder(&owner, owner.root(), "Department").await;
        let hr = env.folder(&owner, &department, "HR").await;
        let file = env.stored_file(&owner, &hr, "contracts.txt", b"confidential").await;
        env.grant(&department, &manager, "manager").await;
        let mut c = env.st.db.acquire().await.unwrap();
        let node = tree::get_node(&mut c, &file).await.unwrap().unwrap();
        assert_eq!(tree::role_on(&mut c, &manager, &node).await.unwrap(), Some(tree::Role::Manager));
        drop(c);
        let change_req = || Json(Change { restricted: true, expected: false, confirmed: true });
        assert!(change(State(env.st.clone()), manager.clone(), Path(hr.clone()), change_req()).await.is_err());
        let _ = change(State(env.st.clone()), owner.clone(), Path(hr.clone()), change_req()).await.unwrap();
        let mut c = env.st.db.acquire().await.unwrap();
        assert_eq!(tree::role_on(&mut c, &manager, &node).await.unwrap(), None);
        assert_eq!(tree::role_on(&mut c, &owner, &node).await.unwrap(), Some(tree::Role::Owner));
        let sql = format!("SELECT COUNT(*) FROM nodes n WHERE n.id = ?1 AND {}", tree::visible_sql(&manager, "n.id"));
        let (count,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&file).fetch_one(&mut *c).await.unwrap();
        assert_eq!(count, 0);
        drop(c);
        env.grant(&hr, &manager, "viewer").await;
        let mut c = env.st.db.acquire().await.unwrap();
        assert_eq!(tree::role_on(&mut c, &manager, &node).await.unwrap(), Some(tree::Role::Viewer));
        assert!(!tree::within_publication(&mut c, &department, &file).await.unwrap());
        assert!(tree::within_publication(&mut c, &hr, &file).await.unwrap());
        assert!(tree::within_publication(&mut c, &hr, &hr).await.unwrap());
        let scopes = tree::saved_scopes(&mut c, Some(&file)).await.unwrap();
        let sql = format!("SELECT {}", tree::scopes_visible_sql(manager.id, "?1"));
        let (visible,): (bool,) = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&scopes).fetch_one(&mut *c).await.unwrap();
        assert!(visible);
        drop(c);
        env.revoke(&hr, &manager).await;
        let mut c = env.st.db.acquire().await.unwrap();
        let (visible,): (bool,) = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&scopes).fetch_one(&mut *c).await.unwrap();
        assert!(!visible);
    }
}

#[cfg(test)]
#[path = "restrictions_tests.rs"]
mod regressions;
