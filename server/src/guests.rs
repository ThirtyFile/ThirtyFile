//! Guest access and sharing policy. Checks run in the caller's write transaction, using current membership.

use crate::{
    auth::{self, Admin, User},
    error::{AppError, AppResult},
    state::AppState,
    tree::{PrincipalType, Role},
    util::now,
};
use axum::{
    Json,
    extract::{Path, State},
};
use serde::Serialize;
use sqlx::SqliteConnection;

pub const DEFAULT_DAYS: i64 = 90;
pub const MAX_DAYS: i64 = 3650;

pub async fn max_days(conn: &mut SqliteConnection) -> AppResult<i64> {
    let value: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = 'guest_max_days'").fetch_optional(conn).await?;
    Ok(value.and_then(|(s,)| s.parse().ok()).unwrap_or(DEFAULT_DAYS).clamp(1, MAX_DAYS))
}

pub async fn own_groups(conn: &mut SqliteConnection) -> AppResult<bool> {
    let (on,): (bool,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM settings WHERE key = 'share_own_groups' AND value = '1')").fetch_one(conn).await?;
    Ok(on)
}

pub async fn target_has_guests(conn: &mut SqliteConnection, kind: PrincipalType, id: i64) -> AppResult<bool> {
    let sql = match kind {
        PrincipalType::User => "SELECT EXISTS(SELECT 1 FROM users WHERE id = ? AND guest = 1)",
        PrincipalType::Group => "SELECT EXISTS(SELECT 1 FROM group_members m JOIN users u ON u.id = m.user_id WHERE m.group_id = ? AND u.guest = 1)",
        PrincipalType::Everyone => return Ok(false),
    };
    let (guest,): (bool,) = sqlx::query_as(sql).bind(id).fetch_one(conn).await?;
    Ok(guest)
}

/// How far past a guest's longest access an end date may be and still be taken (at the longest): the clock of the
/// computer asking may be a little ahead of the server's
const GUEST_SLACK: i64 = 3600;

/// Checks a grant, and gives the end date to store: a guest's end date a little past the longest access is that
/// longest access
pub async fn validate_grant(conn: &mut SqliteConnection, actor: &User, kind: PrincipalType, id: i64, role: Role, expiry: Option<i64>) -> AppResult<Option<i64>> {
    if actor.guest {
        return Err(AppError::forbidden("Guests can't give people access"));
    }
    if !actor.is_admin() && own_groups(conn).await? {
        let allowed = match kind {
            PrincipalType::Everyone => false,
            PrincipalType::User => {
                let (yes,): (bool,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM group_members mine JOIN group_members other ON other.group_id = mine.group_id WHERE mine.user_id = ? AND other.user_id = ?)")
                    .bind(actor.id).bind(id).fetch_one(&mut *conn).await?;
                yes
            }
            PrincipalType::Group => {
                let (yes,): (bool,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM group_members WHERE user_id = ? AND group_id = ?)")
                    .bind(actor.id)
                    .bind(id)
                    .fetch_one(&mut *conn)
                    .await?;
                yes
            }
        };
        if !allowed {
            return Err(AppError::forbidden("You can only give access to members of your own groups"));
        }
    }
    if target_has_guests(conn, kind, id).await? {
        if role > Role::Editor {
            return Err(AppError::bad_request("Guests can't be managers or owners, including through a group"));
        }
        let days = max_days(conn).await?;
        let latest = now() + days * 86400;
        if expiry.is_none_or(|t| t <= now() || t > latest + GUEST_SLACK) {
            return Err(AppError::bad_request(format!("Access given to guests needs an end date within {days} days")));
        }
        return Ok(expiry.map(|t| t.min(latest)));
    }
    Ok(expiry)
}

#[derive(Serialize, sqlx::FromRow)]
pub struct UnsafeGroup {
    pub group_id: i64,
    pub group_name: String,
    pub grant_id: i64,
    pub item_name: String,
    pub role: Role,
    pub expires_at: Option<i64>,
}

async fn unsafe_groups(conn: &mut SqliteConnection, groups: &[i64]) -> AppResult<Vec<UnsafeGroup>> {
    let end = now() + max_days(conn).await? * 86400;
    Ok(sqlx::query_as("SELECT p.id AS group_id, p.name AS group_name, g.id AS grant_id, CASE WHEN n.parent_id IS NULL THEN d.name ELSE n.name END AS item_name, g.role, g.expires_at FROM grants g JOIN groups p ON p.id = g.principal_id JOIN nodes n ON n.id = g.node_id JOIN drives d ON d.id = n.drive_id WHERE g.principal_type = 'group' AND g.principal_id IN (SELECT value FROM json_each(?)) AND (g.expires_at IS NULL OR g.expires_at > ?) AND (g.role IN ('manager','owner') OR g.expires_at IS NULL OR g.expires_at > ?) ORDER BY p.name, g.id")
        .bind(serde_json::to_string(groups).map_err(AppError::internal)?).bind(now()).bind(end).fetch_all(conn).await?)
}

pub async fn validate_guest_groups(conn: &mut SqliteConnection, groups: &[i64]) -> AppResult<()> {
    require_safe_groups(&unsafe_groups(conn, groups).await?)
}

/// Adding a guest to a group must not turn an old unrestricted group grant into guest access.
pub async fn validate_group_join(conn: &mut SqliteConnection, user_id: i64, group_id: i64) -> AppResult<()> {
    if target_has_guests(conn, PrincipalType::User, user_id).await? {
        let problems = unsafe_groups(conn, &[group_id]).await?;
        require_safe_groups(&problems)?;
    }
    Ok(())
}

fn require_safe_groups(problems: &[UnsafeGroup]) -> AppResult<()> {
    if let Some(problem) = problems.first() {
        return Err(AppError::bad_request(format!(
            "Group \"{}\" has access to \"{}\" that guests can't receive. Set its role to Editor or below and an end date within the guest limit first.",
            problem.group_name, problem.item_name
        )));
    }
    Ok(())
}

#[derive(Serialize, sqlx::FromRow)]
pub struct GrantChange {
    pub id: i64,
    pub item_name: String,
    pub old_role: Role,
    pub old_expires_at: Option<i64>,
    #[sqlx(skip)]
    pub role: Option<Role>,
    #[sqlx(skip)]
    pub expires_at: Option<i64>,
}

#[derive(Serialize)]
pub struct ConversionPreview {
    pub revision: String,
    pub max_days: i64,
    pub personal_files: i64,
    pub personal_bytes: i64,
    pub personal_space: bool,
    pub owned_spaces: Vec<String>,
    pub unsafe_groups: Vec<UnsafeGroup>,
    pub changed_grants: Vec<GrantChange>,
    pub share_links: i64,
    pub s3_buckets: i64,
    pub ftp_folders: i64,
}

pub async fn preview_in(conn: &mut SqliteConnection, id: i64) -> AppResult<ConversionPreview> {
    let (personal_space,): (bool,) = sqlx::query_as("SELECT root_id IS NOT NULL OR personal_pending IS NOT NULL FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::not_found("User not found"))?;
    let (personal_files, personal_bytes): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(n.size),0) FROM nodes n JOIN drives d ON d.id = n.drive_id WHERE d.kind = 'personal' AND d.owner_id = ? AND n.kind = 'file'",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    let owned: Vec<(String,)> = sqlx::query_as("SELECT d.name FROM drives d JOIN grants g ON g.node_id = d.root_id WHERE d.kind <> 'personal' AND g.principal_type = 'user' AND g.principal_id = ? AND g.role = 'owner' AND (g.expires_at IS NULL OR g.expires_at > ?) ORDER BY d.name").bind(id).bind(now()).fetch_all(&mut *conn).await?;
    let groups: Vec<(i64,)> = sqlx::query_as("SELECT group_id FROM group_members WHERE user_id = ? ORDER BY group_id").bind(id).fetch_all(&mut *conn).await?;
    let unsafe_groups = unsafe_groups(conn, &groups.into_iter().map(|(g,)| g).collect::<Vec<_>>()).await?;
    let max_days = max_days(conn).await?;
    let end = now() + max_days * 86400;
    let mut changed_grants: Vec<GrantChange> = sqlx::query_as("SELECT g.id, CASE WHEN n.parent_id IS NULL THEN d.name ELSE n.name END AS item_name, g.role AS old_role, g.expires_at AS old_expires_at FROM grants g JOIN nodes n ON n.id = g.node_id JOIN drives d ON d.id = n.drive_id WHERE g.principal_type = 'user' AND g.principal_id = ? AND NOT(d.kind = 'personal' AND d.owner_id = ?) AND (g.expires_at IS NULL OR g.expires_at > ?) AND (g.role IN ('manager','owner') OR g.expires_at IS NULL OR g.expires_at > ?) ORDER BY g.id")
        .bind(id).bind(id).bind(now()).bind(end).fetch_all(&mut *conn).await?;
    for grant in &mut changed_grants {
        grant.role = Some(grant.old_role.min(Role::Editor));
        grant.expires_at = Some(grant.old_expires_at.unwrap_or(end).min(end));
    }
    let (share_links, s3_buckets, ftp_folders): (i64, i64, i64) =
        sqlx::query_as("SELECT (SELECT COUNT(*) FROM shares WHERE owner_id = ?1), (SELECT COUNT(*) FROM s3_buckets WHERE creator_id = ?1), (SELECT COUNT(*) FROM ftp_receive_folders WHERE creator_id = ?1)")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    let owned_spaces: Vec<String> = owned.into_iter().map(|(n,)| n).collect();
    let grants: Vec<_> = changed_grants.iter().map(|g| (g.id, &g.item_name, g.old_role, g.old_expires_at)).collect();
    let revision = crate::util::sha256_hex(serde_json::to_vec(&serde_json::json!({"max_days":max_days,"personal_files":personal_files,"personal_bytes":personal_bytes,"personal_space":personal_space,"owned_spaces":owned_spaces,"unsafe_groups":unsafe_groups,"grants":grants,"share_links":share_links,"s3_buckets":s3_buckets,"ftp_folders":ftp_folders})).map_err(AppError::internal)?.as_slice());
    Ok(ConversionPreview {
        revision,
        max_days,
        personal_files,
        personal_bytes,
        personal_space,
        owned_spaces,
        unsafe_groups,
        changed_grants,
        share_links,
        s3_buckets,
        ftp_folders,
    })
}

pub async fn preview(State(st): State<AppState>, Admin(actor): Admin, Path(id): Path<i64>) -> AppResult<Json<ConversionPreview>> {
    let mut conn = st.db.acquire().await?;
    auth::require_admin(&mut conn, &actor).await?;
    Ok(Json(preview_in(&mut conn, id).await?))
}

/// Called only after the administrator explicitly confirmed the conversion preview.
pub async fn convert(conn: &mut SqliteConnection, id: i64, revision: Option<&str>) -> AppResult<()> {
    let preview = preview_in(conn, id).await?;
    if revision != Some(preview.revision.as_str()) {
        return Err(AppError::conflict("The guest conversion preview has changed. Review it again before confirming."));
    }
    if !preview.owned_spaces.is_empty() {
        return Err(AppError::bad_request(format!("Transfer ownership of these spaces before making this account a guest: {}", preview.owned_spaces.join(", "))));
    }
    require_safe_groups(&preview.unsafe_groups)?;
    for grant in preview.changed_grants {
        sqlx::query("UPDATE grants SET role = ?, expires_at = ? WHERE id = ?").bind(grant.role).bind(grant.expires_at).bind(grant.id).execute(&mut *conn).await?;
    }
    // Keep app passwords and file data. Existing protocol operations recheck the account in their transaction.
    sqlx::query("DELETE FROM sessions WHERE user_id = ?").bind(id).execute(&mut *conn).await?;
    Ok(())
}

#[cfg(test)]
#[path = "guests/tests.rs"]
mod tests;
