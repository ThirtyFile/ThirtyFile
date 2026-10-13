//! Roles and permissions: space and folder grants, the user's role on a node, and what has been shared with them.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;

use super::{Crumb, DRIVE_COLS, Drive, NODE_COLS, Node, get_drive, get_node, resolve_alias};
use crate::{
    auth::User,
    error::{AppError, AppResult},
    util::now,
};

/// Role on a space or folder, from lowest to highest (`grants.role`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    /// Adds items, and changes, moves and deletes only those they added (`allows_on`)
    Contributor,
    Editor,
    Manager,
    Owner,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "viewer" => Some(Role::Viewer),
            "contributor" => Some(Role::Contributor),
            "editor" => Some(Role::Editor),
            "manager" => Some(Role::Manager),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Contributor => "contributor",
            Role::Editor => "editor",
            Role::Manager => "manager",
            Role::Owner => "owner",
        }
    }
}

/// Who a grant gives access to (`grants.principal_type`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum PrincipalType {
    /// One person (`principal_id`)
    User,
    /// The members of a group (`principal_id`)
    Group,
    /// Everyone who can sign in (`principal_id` 0)
    Everyone,
}

impl PrincipalType {
    pub fn parse(s: &str) -> Option<PrincipalType> {
        match s {
            "user" => Some(PrincipalType::User),
            "group" => Some(PrincipalType::Group),
            "everyone" => Some(PrincipalType::Everyone),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            PrincipalType::User => "user",
            PrincipalType::Group => "group",
            PrincipalType::Everyone => "everyone",
        }
    }
}

/// Capability an operation requires
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Read,
    /// Add to a folder: upload, create, move, copy or restore into it
    Add,
    /// Change an item: upload over it, edit, rename
    Write,
    /// Move an item out of its folder
    Move,
    /// Move to trash, delete permanently
    Delete,
    /// Share: create share links, and give people access and manage members (drives.rs); the interface calls it "Share"
    Share,
}

/// Effective capability = role ∩ the user account's own permissions
pub fn allows(user: &User, role: Role, need: Need) -> AppResult<()> {
    let ok = match need {
        Need::Read => true,
        Need::Add => role >= Role::Contributor && (user.can_write || user.is_admin()),
        Need::Write | Need::Move => role >= Role::Editor && (user.can_write || user.is_admin()),
        Need::Delete => role >= Role::Editor && (user.can_delete || user.is_admin()),
        Need::Share => role >= Role::Editor && (user.can_share || user.is_admin()),
    };
    if ok {
        return Ok(());
    }
    Err(AppError::forbidden(match need {
        Need::Read => "You don't have access",
        Need::Add | Need::Write | Need::Move => "You don't have permission to make changes here",
        Need::Delete => "You don't have permission to delete here",
        Need::Share => "You don't have permission to share here",
    }))
}

/// Whether the user added `node` themselves in ThirtyFile: not an item the check of a folder space found on its disk,
/// nor a space's root folder
pub fn is_own(user: &User, node: &Node) -> bool {
    node.owner_id == user.id && !node.found && node.parent_id.is_some()
}

/// `allows`, for an item: a contributor may also change, move and delete what they added themselves (`is_own`), as
/// their account's permissions let them. A folder they made moves or goes to the trash only while everything in it is
/// theirs too, since that goes with it.
pub async fn allows_on(conn: &mut SqliteConnection, user: &User, role: Role, need: Need, node: &Node) -> AppResult<()> {
    let refused = match allows(user, role, need) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    if role != Role::Contributor || !matches!(need, Need::Write | Need::Move | Need::Delete) || !is_own(user, node) {
        return Err(refused);
    }
    allows(user, Role::Editor, need)?;
    if matches!(need, Need::Move | Need::Delete) && node.is_folder() && holds_others(conn, user, node).await? {
        return Err(AppError::forbidden(format!("\"{}\" holds items other people added, so you can't move or delete it", node.name)));
    }
    Ok(())
}

/// Whether anything in the folder `id` (in the trash or not) wasn't added by `user` (`is_own`)
async fn holds_others(conn: &mut SqliteConnection, user: &User, node: &Node) -> AppResult<bool> {
    let (others,): (bool,) = sqlx::query_as(
        "WITH RECURSIVE sub(id) AS (SELECT id FROM nodes WHERE parent_id = ?1 UNION ALL SELECT n.id FROM nodes n JOIN sub ON n.parent_id = sub.id)
         SELECT EXISTS (SELECT 1 FROM nodes n JOIN sub ON n.id = sub.id WHERE n.owner_id <> ?2 OR n.found)",
    )
    .bind(&node.id)
    .bind(user.id)
    .fetch_one(&mut *conn)
    .await?;
    if others || !node.in_folder_space() {
        return Ok(others);
    }
    // The index may not yet know about files made through SMB or on the server. Scan every disk entry, including
    // names ordinary scans skip, in bounded batches. Unknown, unreadable and symbolic-link entries are refused.
    // Only contributors moving/deleting their own folder need this; the folder space's change lock is held by the
    // caller. The disk step times out, and neither a large directory nor the whole tree is collected in memory.
    let (send, mut receive) = tokio::sync::mpsc::channel(1);
    let folder = node.clone();
    let refuse = format!("\"{}\" holds items other people added, so you can't move or delete it", node.name);
    let disk = crate::fsops::on_disk(node.drive(), std::time::Duration::from_secs(5), move || disk_entries(&folder, send));
    let check = async {
        while let Some(paths) = receive.recv().await {
            let (owned,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM nodes WHERE drive_id = ? AND owner_id = ? AND NOT found
                 AND fs_path IN (SELECT value FROM json_each(?))",
            )
            .bind(node.drive())
            .bind(user.id)
            .bind(serde_json::to_string(&paths).map_err(AppError::internal)?)
            .fetch_one(&mut *conn)
            .await?;
            if owned as usize != paths.len() {
                return Err(AppError::forbidden(&refuse));
            }
        }
        Ok::<_, AppError>(())
    };
    tokio::try_join!(disk, check)?;
    Ok(false)
}

/// Enumerates disk paths without following links. A channel holds at most one batch, and traversal keeps at most
/// 128 directory iterators open. Dropping the receiver after a refusal stops the producer before its next entry.
fn disk_entries(node: &Node, send: tokio::sync::mpsc::Sender<Vec<String>>) -> AppResult<()> {
    let refused = || AppError::forbidden(format!("\"{}\" holds items other people added, so you can't move or delete it", node.name));
    let root = node.fs_pinned().and_then(|p| p.dir()).map_err(|_| refused())?;
    let entries = std::fs::read_dir(root.as_path()).map_err(|_| refused())?;
    let mut stack = vec![(root, node.fs_path.clone().ok_or_else(refused)?, entries)];
    let mut batch = Vec::with_capacity(500);
    while let Some((dir, rel, entries)) = stack.last_mut() {
        if send.is_closed() {
            return Ok(());
        }
        let Some(entry) = entries.next() else {
            stack.pop();
            continue;
        };
        let entry = entry.map_err(|_| refused())?;
        let name = entry.file_name().into_string().map_err(|_| refused())?;
        let child = dir.join(&name).map_err(|_| refused())?;
        let meta = std::fs::symlink_metadata(child.as_path()).map_err(|_| refused())?;
        if meta.file_type().is_symlink() || (!meta.is_file() && !meta.is_dir()) {
            return Err(refused());
        }
        let path = format!("{rel}/{name}");
        batch.push(path.clone());
        if batch.len() == 500 && send.blocking_send(std::mem::replace(&mut batch, Vec::with_capacity(500))).is_err() {
            return Ok(());
        }
        if meta.is_dir() {
            if stack.len() >= 128 {
                return Err(refused());
            }
            let child = child.dir().map_err(|_| refused())?;
            let entries = std::fs::read_dir(child.as_path()).map_err(|_| refused())?;
            stack.push((child, path, entries));
        }
    }
    if !batch.is_empty() {
        let _ = send.blocking_send(batch);
    }
    Ok(())
}

impl super::LinkMakers {
    /// Whether someone with this role on an item may make a share link of it
    pub fn allow(self, role: Role) -> bool {
        match self {
            super::LinkMakers::Editors => role >= Role::Editor,
            super::LinkMakers::Managers => role >= Role::Manager,
            super::LinkMakers::Nobody => false,
        }
    }
}

/// Whether the user may make a share link of an item they have this role on in this space: their permission to share
/// (`allows`), and whom the space lets make links. A personal space's owner always decides alone.
pub fn may_link(user: &User, role: Role, drive: &super::Drive) -> AppResult<()> {
    allows(user, role, Need::Share)?;
    if drive.kind == super::SpaceKind::Personal || drive.link_makers.allow(role) {
        return Ok(());
    }
    Err(AppError::forbidden(match drive.link_makers {
        super::LinkMakers::Nobody => "Share links can't be made in this space",
        _ => "In this space, only managers can make share links",
    }))
}

/// SQL for "the grant `g` applies to the user and hasn't expired", with the numbers of the query's parameters holding
/// the user's id and the current time
fn principal_match(user: u8, now: u8) -> String {
    format!(
        "(g.expires_at IS NULL OR g.expires_at > ?{now})
    AND (g.principal_type = 'everyone'
      OR (g.principal_type = 'user' AND g.principal_id = ?{user})
      OR (g.principal_type = 'group' AND g.principal_id IN (SELECT group_id FROM group_members WHERE user_id = ?{user})))"
    )
}

/// The user's role on a node: the highest grant among all ancestors (including itself); None means no access
pub async fn role_on(conn: &mut SqliteConnection, user: &User, node: &Node) -> AppResult<Option<Role>> {
    Ok(access(conn, user, node).await?.0)
}

/// The user's role on a node that is out of the trash, as is every folder it is in; None means no access, or that it
/// is in the trash. A folder moved to the trash is marked at once, but what is in a large one only a batch at a time
/// (tree/changes.rs): meanwhile its items count as in the trash too, so nothing can be saved into them.
pub async fn live_role_on(conn: &mut SqliteConnection, user: &User, node: &Node) -> AppResult<Option<Role>> {
    let (role, in_trash) = access(conn, user, node).await?;
    Ok(role.filter(|_| !in_trash))
}

/// The user's role on a node (see `role_on`), and whether it or a folder it is in is in the trash
async fn access(conn: &mut SqliteConnection, user: &User, node: &Node) -> AppResult<(Option<Role>, bool)> {
    let Some(drive) = get_drive(conn, node.drive()).await? else { return Ok((None, false)) };
    if drive.disabled {
        return Ok((None, false));
    }
    // Administrators manage the company shared space (personal spaces are unaffected, for privacy)
    let mut best = (drive.kind == super::SpaceKind::Company && user.is_admin()).then_some(Role::Manager);
    if user.is_admin() && drive.kind != super::SpaceKind::Personal {
        if let Some(root) = &user.protocol_root {
            let (permitted,): (bool,) = sqlx::query_as("WITH RECURSIVE up(id,parent_id,trash) AS (SELECT id,parent_id,trashed_at IS NOT NULL FROM nodes WHERE id=?1 UNION ALL SELECT n.id,n.parent_id,n.trashed_at IS NOT NULL FROM nodes n JOIN up ON n.id=up.parent_id) SELECT EXISTS(SELECT 1 FROM up WHERE id=?2) AND NOT EXISTS(SELECT 1 FROM up WHERE trash)")
                .bind(&node.id).bind(root).fetch_one(&mut *conn).await?;
            if permitted {
                best = Some(Role::Manager);
            }
        }
    }
    // A row for each folder on the way up, and one more for each grant there that applies
    let sql = format!(
        "WITH RECURSIVE up(id, parent_id, trashed) AS (
           SELECT id, parent_id, trashed_at IS NOT NULL FROM nodes WHERE id = ?1
           UNION ALL SELECT n.id, n.parent_id, n.trashed_at IS NOT NULL FROM nodes n JOIN up ON n.id = up.parent_id
         )
         SELECT g.role, up.trashed FROM up LEFT JOIN grants g ON g.node_id = up.id AND {}",
        principal_match(2, 3)
    );
    let rows: Vec<(Option<Role>, bool)> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&node.id).bind(user.id).bind(now()).fetch_all(conn).await?;
    let mut in_trash = false;
    for (r, trashed) in rows {
        best = best.max(r);
        in_trash |= trashed;
    }
    Ok((best, in_trash))
}

/// When the user's right to manage access to a node ends: the latest end of the grants that make them a manager or
/// owner there; None when one of them doesn't end (or they manage it through no grant)
pub async fn manages_until(conn: &mut SqliteConnection, user: &User, node: &Node) -> AppResult<Option<i64>> {
    let sql = format!(
        "WITH RECURSIVE up(id, parent_id) AS (
           SELECT id, parent_id FROM nodes WHERE id = ?1
           UNION ALL SELECT n.id, n.parent_id FROM nodes n JOIN up ON n.id = up.parent_id
         )
         SELECT CASE WHEN COUNT(*) = COUNT(g.expires_at) THEN MAX(g.expires_at) END
         FROM grants g JOIN up ON g.node_id = up.id WHERE g.role IN ('manager', 'owner') AND {}",
        principal_match(2, 3)
    );
    let (until,): (Option<i64>,) = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&node.id).bind(user.id).bind(now()).fetch_one(conn).await?;
    Ok(until)
}

/// Whether a grant to this principal gives the user access: it names them, a group of theirs, or everyone
pub async fn grant_applies_to(conn: &mut SqliteConnection, user: &User, principal_type: PrincipalType, principal_id: i64) -> AppResult<bool> {
    Ok(match principal_type {
        PrincipalType::Everyone => true,
        PrincipalType::User => principal_id == user.id,
        PrincipalType::Group => {
            let (n,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM group_members WHERE group_id = ? AND user_id = ?").bind(principal_id).bind(user.id).fetch_one(conn).await?;
            n > 0
        }
    })
}

/// Gets a node the user can access that isn't in the trash (nor in a folder that is), together with the user's role.
/// `root` means the user's own personal space, `shared` means the "All files" company space.
pub async fn node_with_role(conn: &mut SqliteConnection, user: &User, id: &str) -> AppResult<(Node, Role)> {
    let id = resolve_alias(user, id)?;
    if let Some(n) = get_node(conn, id).await?
        && n.trashed_at.is_none()
        && let Some(role) = live_role_on(conn, user, &n).await?
    {
        return Ok((n, role));
    }
    Err(AppError::not_found("Item not found"))
}

/// Gets a node and checks the required capability
pub async fn node_for(conn: &mut SqliteConnection, user: &User, id: &str, need: Need) -> AppResult<Node> {
    let current = if user.protocol_key.is_some() { Some(crate::s3::credentials::actor_in_transaction(conn, user, need).await?) } else { None };
    let user = current.as_ref().unwrap_or(user);
    let (n, role) = node_with_role(conn, user, id).await?;
    if let Some(root) = &user.protocol_root {
        if !crate::access::inside(conn, root, &n.id).await? {
            return Err(AppError::forbidden("Object is outside the bucket"));
        }
    }
    allows_on(conn, user, role, need, &n).await?;
    if matches!(need, Need::Add | Need::Write | Need::Move | Need::Delete) && n.space_read_only {
        return Err(read_only_error(&n));
    }
    Ok(n)
}

/// A read-only space can be browsed, downloaded and shared
pub fn read_only_space() -> AppError {
    AppError::forbidden("This space is read-only")
}

/// Why the space of `n` can't be changed: it is read-only, or being moved to another storage location
pub fn read_only_error(n: &Node) -> AppError {
    if n.space_moving {
        AppError::forbidden("This space is being moved to another storage location. It is read-only until the move finishes.")
    } else {
        read_only_space()
    }
}

pub async fn folder_for(conn: &mut SqliteConnection, user: &User, id: &str, need: Need) -> AppResult<Node> {
    let n = node_for(conn, user, id, need).await?;
    if !n.is_folder() {
        return Err(AppError::bad_request("The destination isn't a folder"));
    }
    Ok(n)
}

/// Read access is enough
pub async fn owned_node(conn: &mut SqliteConnection, user: &User, id: &str) -> AppResult<Node> {
    node_for(conn, user, id, Need::Read).await
}

/// Spaces the user can access (space-level grants) and their roles
pub async fn user_drives(conn: &mut SqliteConnection, user: &User) -> AppResult<Vec<(Drive, Role)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        drive: Drive,
        role: Role,
    }
    let sql = format!(
        "SELECT {DRIVE_COLS}, g.role FROM drives d JOIN grants g ON g.node_id = d.root_id
         WHERE d.disabled = 0 AND {}",
        principal_match(1, 2)
    );
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(user.id).bind(now()).fetch_all(&mut *conn).await?;
    let mut map: HashMap<String, (Drive, Role)> = HashMap::new();
    for r in rows {
        let role = r.role;
        map.entry(r.drive.id.clone()).and_modify(|e| e.1 = e.1.max(role)).or_insert((r.drive, role));
    }
    if user.is_admin() {
        let sql = format!("SELECT {DRIVE_COLS} FROM drives d WHERE d.kind = 'company' AND d.disabled = 0");
        let company: Vec<Drive> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).fetch_all(&mut *conn).await?;
        for d in company {
            map.entry(d.id.clone()).and_modify(|e| e.1 = e.1.max(Role::Manager)).or_insert((d, Role::Manager));
        }
    }
    let mut out: Vec<(Drive, Role)> = map.into_values().collect();
    out.sort_by(|a, b| a.0.kind.cmp(&b.0.kind).then_with(|| a.0.name.cmp(&b.0.name)));
    Ok(out)
}

/// Ids of the spaces the user is a member of
pub async fn member_of(conn: &mut SqliteConnection, user: &User) -> AppResult<Vec<String>> {
    Ok(user_drives(conn, user).await?.into_iter().map(|(d, _)| d.id).collect())
}

/// Folders / files others shared with me (excluding items in spaces I'm already a member of): (node, role, sharer)
pub async fn shared_with_me(conn: &mut SqliteConnection, user: &User) -> AppResult<Vec<(Node, Role, String)>> {
    let member_of = member_of(conn, user).await?;
    shared_with_me_outside(conn, user, &member_of).await
}

/// Ids of the items shared with the user in spaces they aren't a member of (`member_of`)
pub async fn shared_ids(conn: &mut SqliteConnection, user: &User, member_of: &[String]) -> AppResult<HashSet<String>> {
    Ok(shared_with_me_outside(conn, user, member_of).await?.into_iter().map(|(n, _, _)| n.id).collect())
}

/// For someone who only has a folder of the space shared with them: where `path` (from the root down) becomes visible
/// to them, at the first shared folder in it; None when it doesn't pass one
pub fn shared_start(path: &[Crumb], shared: &HashSet<String>) -> Option<usize> {
    path.iter().position(|c| shared.contains(&c.id))
}

/// Same as `shared_with_me`, for callers that already have the user's space list (saves the grants query)
pub async fn shared_with_me_outside(conn: &mut SqliteConnection, user: &User, member_of: &[String]) -> AppResult<Vec<(Node, Role, String)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        node: Node,
        role: Role,
        sharer: String,
    }
    let sql = format!(
        "SELECT {NODE_COLS}, g.role, COALESCE((SELECT username FROM users WHERE id = g.granted_by), '') AS sharer
         FROM grants g JOIN nodes n ON n.id = g.node_id JOIN drives d ON d.id = n.drive_id
         WHERE n.parent_id IS NOT NULL AND n.trashed_at IS NULL AND d.disabled = 0 AND {}
         ORDER BY g.created_at DESC",
        principal_match(1, 2)
    );
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(user.id).bind(now()).fetch_all(&mut *conn).await?;
    let mut map: HashMap<String, (Node, Role, String)> = HashMap::new();
    for r in rows {
        let role = r.role;
        if member_of.iter().any(|d| d == r.node.drive()) {
            continue;
        }
        map.entry(r.node.id.clone()).and_modify(|e| e.1 = e.1.max(role)).or_insert((r.node, role, r.sharer));
    }
    Ok(map.into_values().collect())
}

/// Builds the SQL condition "node is within the user's accessible scope"; the bound values are two JSON arrays (space ids, shared folder ids)
pub fn scope_sql(drives_param: usize, folders_param: usize) -> String {
    format!(
        "(n.drive_id IN (SELECT value FROM json_each(?{drives_param}))
          OR n.id IN (WITH RECURSIVE s(id) AS (
                SELECT value FROM json_each(?{folders_param})
                UNION ALL SELECT c.id FROM nodes c JOIN s ON c.parent_id = s.id
             ) SELECT id FROM s))"
    )
}

/// The user's accessible scope: (space id JSON, shared folder id JSON)
pub async fn scope(conn: &mut SqliteConnection, user: &User) -> AppResult<(String, String)> {
    let drives = member_of(conn, user).await?;
    let folders: Vec<String> = shared_ids(conn, user, &drives).await?.into_iter().collect();
    Ok((serde_json::to_string(&drives).unwrap(), serde_json::to_string(&folders).unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    async fn role(env: &testutil::TestEnv, user: &User, id: &str) -> Option<Role> {
        let mut c = env.st.db.acquire().await.unwrap();
        let node = get_node(&mut c, id).await.unwrap().unwrap();
        role_on(&mut c, user, &node).await.unwrap()
    }

    #[tokio::test]
    async fn role_on_follows_drives_and_folder_shares() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let admin = env.admin().await;
        let project = env.folder(&amy, amy.root(), "Projects").await;
        let sub = env.folder(&amy, &project, "Subfolder").await;
        let private = env.folder(&amy, amy.root(), "Private").await;

        // Personal space: only the owner; administrators can't see it either (for privacy)
        assert_eq!(role(&env, &amy, &project).await, Some(Role::Owner));
        assert_eq!(role(&env, &ben, &project).await, None);
        assert_eq!(role(&env, &admin, &project).await, None);

        // Folder sharing: grants are inherited downward without affecting sibling folders or the space root
        env.grant(&project, &ben, "editor").await;
        assert_eq!(role(&env, &ben, &sub).await, Some(Role::Editor));
        assert_eq!(role(&env, &ben, &private).await, None);
        assert_eq!(role(&env, &ben, amy.root()).await, None);

        // The higher role wins
        env.grant(&sub, &ben, "manager").await;
        assert_eq!(role(&env, &ben, &sub).await, Some(Role::Manager));

        // Takes effect immediately after revocation
        env.revoke(&project, &ben).await;
        env.revoke(&sub, &ben).await;
        assert_eq!(role(&env, &ben, &sub).await, None);

        // Expired grants are ignored
        let mut c = env.st.db.acquire().await.unwrap();
        crate::db::add_grant(&mut c, &project, "user", ben.id, "viewer", None, Some(now() - 10)).await.unwrap();
        drop(c);
        assert_eq!(role(&env, &ben, &project).await, None);

        // Company shared space: everyone can edit, administrators can manage
        let company = env.st.shared_root().unwrap();
        assert_eq!(role(&env, &ben, &company).await, Some(Role::Editor));
        assert_eq!(role(&env, &admin, &company).await, Some(Role::Manager));
    }
}
