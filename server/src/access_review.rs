//! Access review (Control panel › Access review): who can reach what, on one page, for administrators to go through
//! now and then, as audits ask.
//!
//! - Spaces: each team space and the company space, its members and their roles, the folders in it shared on their own
//!   or restricted (and who has access there), as grants say;
//! - people: each account, what the grants that name it, its groups or everyone give it, its app passwords and SSH keys;
//! - outside access: share links, S3 buckets and their keys, FTP receive folders;
//! - grants with an end date, soonest first.
//!
//! What has ended (a grant, a link, a key past its end date) is left out. Personal spaces stay private, as everywhere
//! for administrators: an item in one is shown as an item of its owner's personal space, never by name or path.
//! Administrators' own access (they manage team and company spaces without a grant) isn't a grant, and isn't listed.
//!
//! A reminder (`every_days`) tells administrators when a review is due; marking one reviewed is in the activity log.

use std::collections::{BTreeMap, HashMap};

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqliteConnection;

use crate::{
    auth::Admin,
    db,
    error::{AppError, AppResult},
    state::AppState,
    util::now,
};

const SETTING: &str = "access_review";
/// Every so many days at most between reviews (a year and a day)
const MAX_DAYS: u32 = 366;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Reminder {
    /// Ask for a review every so many days; 0: never
    pub every_days: u32,
    /// Since when it was asked for (the start of the first period)
    pub since: Option<i64>,
    pub last_reviewed_at: Option<i64>,
    pub last_reviewed_by: Option<String>,
    /// What administrators were last told (`backups::scheduler::Alert`)
    pub alerted: String,
}

impl Reminder {
    /// When the next review is due, when one is asked for
    pub fn due_at(&self) -> Option<i64> {
        (self.every_days > 0).then(|| self.last_reviewed_at.or(self.since).unwrap_or_else(now) + i64::from(self.every_days) * 86400)
    }
}

// ───────────── What the page shows ─────────────

#[derive(Serialize, Clone)]
pub struct Principal {
    /// "user", "group" or "everyone"
    pub kind: String,
    pub id: i64,
    pub name: String,
    /// A guest, or a group with a guest in it
    pub guest: bool,
}

#[derive(Serialize, Clone)]
pub struct Grant {
    pub principal: Principal,
    pub role: String,
    pub expires_at: Option<i64>,
    pub granted_at: i64,
}

/// Where something is: its space, and its folders below the space's top (none for the space itself). In a personal
/// space, only whose space it is: `path` and `node_id` are left out
#[derive(Serialize, Clone)]
pub struct Place {
    pub space_id: String,
    /// The space's name, or for a personal space its owner's username
    pub space: String,
    pub space_kind: String,
    pub node_id: Option<String>,
    pub path: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct Folder {
    pub place: Place,
    pub restricted: bool,
    pub grants: Vec<Grant>,
}

#[derive(Serialize)]
pub struct Space {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub members: Vec<Grant>,
    /// Folders and files shared on their own, and restricted folders, in path order
    pub folders: Vec<Folder>,
}

/// What a personal space shares, in numbers only
#[derive(Serialize)]
pub struct PersonalSharing {
    pub owner: String,
    pub items: i64,
    pub grants: i64,
    pub links: i64,
}

#[derive(Serialize)]
pub struct Expiring {
    pub place: Place,
    pub grant: Grant,
}

#[derive(Serialize)]
pub struct Reach {
    pub place: Place,
    pub role: String,
    /// "direct", "group" (with `group`) or "everyone"
    pub via: String,
    pub group: Option<String>,
    pub expires_at: Option<i64>,
}

#[derive(Serialize, Clone, sqlx::FromRow)]
pub struct AppPassword {
    pub name: String,
    pub scope: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
}

#[derive(Serialize, Clone, sqlx::FromRow)]
pub struct SshKey {
    pub name: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Serialize)]
pub struct Person {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub guest: bool,
    pub admin: bool,
    pub disabled: bool,
    pub groups: Vec<String>,
    pub reach: Vec<Reach>,
    pub app_passwords: Vec<AppPassword>,
    pub ssh_keys: Vec<SshKey>,
}

#[derive(Serialize)]
pub struct Link {
    pub place: Place,
    pub creator: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub password: bool,
    pub allow_upload: bool,
    pub allow_download: bool,
    pub downloads: i64,
    pub max_downloads: Option<i64>,
    pub views: i64,
    pub last_access: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct BucketKey {
    pub description: String,
    pub writable: bool,
    pub enabled: bool,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
}

#[derive(Serialize)]
pub struct Bucket {
    pub name: String,
    pub place: Place,
    pub creator: String,
    pub enabled: bool,
    pub keys: Vec<BucketKey>,
}

#[derive(Serialize)]
pub struct ReceiveFolder {
    pub name: String,
    pub username: String,
    pub place: Place,
    pub creator: String,
    pub enabled: bool,
    pub allow_list: bool,
    pub allow_read: bool,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
}

#[derive(Serialize)]
pub struct ReviewState {
    pub every_days: u32,
    pub last_reviewed_at: Option<i64>,
    pub last_reviewed_by: Option<String>,
    pub due_at: Option<i64>,
}

#[derive(Serialize)]
pub struct Overview {
    pub generated_at: i64,
    pub spaces: Vec<Space>,
    pub personal: Vec<PersonalSharing>,
    pub expiring: Vec<Expiring>,
    pub people: Vec<Person>,
    pub links: Vec<Link>,
    pub buckets: Vec<Bucket>,
    pub receive_folders: Vec<ReceiveFolder>,
    pub review: ReviewState,
}

// ───────────── Gathering it ─────────────

#[derive(sqlx::FromRow)]
struct GrantRow {
    node_id: String,
    principal_type: String,
    principal_id: i64,
    role: String,
    expires_at: Option<i64>,
    created_at: i64,
    principal_name: String,
    guest: bool,
}

#[derive(sqlx::FromRow, Clone)]
struct NodeRow {
    id: String,
    at_root: bool,
    restricted: bool,
    drive_id: String,
    drive_name: String,
    drive_kind: String,
    owner_name: Option<String>,
}

/// The places of nodes, each path looked up once
struct Places {
    nodes: HashMap<String, NodeRow>,
    paths: HashMap<String, Vec<String>>,
}

impl Places {
    async fn of(&mut self, conn: &mut SqliteConnection, id: &str) -> AppResult<Option<Place>> {
        let Some(n) = self.nodes.get(id).cloned() else { return Ok(None) };
        if n.drive_kind == "personal" {
            return Ok(Some(Place { space_id: n.drive_id, space: n.owner_name.unwrap_or_default(), space_kind: n.drive_kind, node_id: None, path: None }));
        }
        if !self.paths.contains_key(id) {
            let path = crate::tree::path_of(conn, id).await?.into_iter().map(|c| c.name).collect();
            self.paths.insert(id.to_string(), path);
        }
        Ok(Some(Place { space_id: n.drive_id, space: n.drive_name, space_kind: n.drive_kind, node_id: Some(n.id), path: Some(self.paths[id].clone()) }))
    }
}

/// The nodes (not in the trash, in spaces that are on) that `ids` name
async fn nodes(conn: &mut SqliteConnection, ids: Vec<String>) -> AppResult<HashMap<String, NodeRow>> {
    let rows: Vec<NodeRow> = sqlx::query_as(
        "SELECT n.id, n.parent_id IS NULL AS at_root, n.restricted, n.drive_id, d.name AS drive_name, d.kind AS drive_kind,
           (SELECT username FROM users WHERE id = d.owner_id) AS owner_name
         FROM nodes n JOIN drives d ON d.id = n.drive_id
         WHERE n.id IN (SELECT value FROM json_each(?1)) AND n.trashed_at IS NULL AND d.disabled = 0",
    )
    .bind(serde_json::to_string(&ids).map_err(AppError::internal)?)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|n| (n.id.clone(), n)).collect())
}

fn grant_of(g: &GrantRow) -> Grant {
    Grant {
        principal: Principal { kind: g.principal_type.clone(), id: g.principal_id, name: g.principal_name.clone(), guest: g.guest },
        role: g.role.clone(),
        expires_at: g.expires_at,
        granted_at: g.created_at,
    }
}

/// Whether a grant applies to a guest: the rule of `tree::permissions::principal_sql`
fn reaches_guest(g: &GrantRow) -> bool {
    g.expires_at.is_some() && matches!(g.role.as_str(), "viewer" | "contributor" | "editor")
}

#[derive(sqlx::FromRow)]
struct ShareRow {
    node_id: String,
    creator: String,
    created_at: i64,
    expires_at: Option<i64>,
    password: bool,
    allow_upload: bool,
    allow_download: bool,
    downloads: i64,
    max_downloads: Option<i64>,
    views: i64,
    last_access: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct BucketRow {
    id: String,
    name: String,
    node_id: String,
    creator: String,
    enabled: bool,
}

#[derive(sqlx::FromRow)]
struct FtpRow {
    name: String,
    username: String,
    node_id: String,
    creator: String,
    enabled: bool,
    allow_list: bool,
    allow_read: bool,
    expires_at: Option<i64>,
    last_used_at: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: i64,
    username: String,
    display_name: String,
    guest: bool,
    role: String,
    disabled: bool,
}

#[derive(sqlx::FromRow)]
struct AppPasswordRow {
    user_id: i64,
    #[sqlx(flatten)]
    password: AppPassword,
}

#[derive(sqlx::FromRow)]
struct SshKeyRow {
    user_id: i64,
    #[sqlx(flatten)]
    key: SshKey,
}

/// What reaches outside: links that still work, buckets and receive folders that haven't ended
struct Outside {
    shares: Vec<ShareRow>,
    buckets: Vec<BucketRow>,
    ftp: Vec<FtpRow>,
}

/// Grants that haven't ended, with who they name
async fn load_grants(conn: &mut SqliteConnection, t: i64) -> AppResult<Vec<GrantRow>> {
    Ok(sqlx::query_as(
        "SELECT g.node_id, g.principal_type, g.principal_id, g.role, g.expires_at, g.created_at,
           CASE g.principal_type WHEN 'user' THEN COALESCE(u.username, '(deleted)') WHEN 'group' THEN COALESCE(gr.name, '(deleted)') ELSE 'Everyone' END AS principal_name,
           CASE g.principal_type WHEN 'user' THEN COALESCE(u.guest, 0)
             WHEN 'group' THEN EXISTS(SELECT 1 FROM group_members m JOIN users mu ON mu.id = m.user_id WHERE m.group_id = g.principal_id AND mu.guest = 1)
             ELSE 0 END AS guest
         FROM grants g
         LEFT JOIN users u ON g.principal_type = 'user' AND u.id = g.principal_id
         LEFT JOIN groups gr ON g.principal_type = 'group' AND gr.id = g.principal_id
         WHERE g.expires_at IS NULL OR g.expires_at > ?1
         ORDER BY g.created_at, g.id",
    )
    .bind(t)
    .fetch_all(conn)
    .await?)
}

async fn load_outside(conn: &mut SqliteConnection, t: i64) -> AppResult<Outside> {
    let shares = sqlx::query_as(
        "SELECT s.node_id, COALESCE(u.username, '(deleted)') AS creator, s.created_at, s.expires_at, s.password_hash IS NOT NULL AS password,
           s.allow_upload, s.allow_download, s.downloads, s.max_downloads, s.views, s.last_access
         FROM shares s LEFT JOIN users u ON u.id = s.owner_id
         WHERE (s.expires_at IS NULL OR s.expires_at > ?1) AND (s.max_downloads IS NULL OR s.downloads < s.max_downloads)
         ORDER BY s.created_at DESC",
    )
    .bind(t)
    .fetch_all(&mut *conn)
    .await?;
    let buckets = sqlx::query_as(
        "SELECT b.id, b.name, b.node_id, COALESCE(u.username, '(deleted)') AS creator, b.enabled FROM s3_buckets b LEFT JOIN users u ON u.id = b.creator_id ORDER BY b.name",
    )
    .fetch_all(&mut *conn)
    .await?;
    let ftp = sqlx::query_as(
        "SELECT f.name, f.username, f.node_id, COALESCE(u.username, '(deleted)') AS creator, f.enabled, f.allow_list, f.allow_read, f.expires_at, f.last_used_at
         FROM ftp_receive_folders f LEFT JOIN users u ON u.id = f.creator_id
         WHERE f.expires_at IS NULL OR f.expires_at > ?1 ORDER BY f.name",
    )
    .bind(t)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Outside { shares, buckets, ftp })
}

/// The ids of every node the review mentions: the grants', the restricted folders', and those of links, buckets and
/// receive folders
async fn mentioned(conn: &mut SqliteConnection, grants: &[GrantRow], outside: &Outside) -> AppResult<Vec<String>> {
    let (restricted,): (String,) =
        sqlx::query_as("SELECT COALESCE(json_group_array(id), '[]') FROM nodes WHERE restricted = 1 AND trashed_at IS NULL").fetch_one(conn).await?;
    let mut ids: Vec<String> = grants.iter().map(|g| g.node_id.clone()).collect();
    ids.extend(serde_json::from_str::<Vec<String>>(&restricted).unwrap_or_default());
    ids.extend(outside.shares.iter().map(|s| s.node_id.clone()));
    ids.extend(outside.buckets.iter().map(|b| b.node_id.clone()));
    ids.extend(outside.ftp.iter().map(|f| f.node_id.clone()));
    ids.sort();
    ids.dedup();
    Ok(ids)
}

type ByNode<'a> = BTreeMap<String, Vec<&'a GrantRow>>;

/// Team and company spaces: their members, and the folders shared on their own or restricted, in path order
async fn spaces(conn: &mut SqliteConnection, places: &mut Places, by_node: &ByNode<'_>) -> AppResult<Vec<Space>> {
    #[derive(sqlx::FromRow)]
    struct DriveRow {
        id: String,
        name: String,
        kind: String,
    }
    let drives: Vec<DriveRow> =
        sqlx::query_as("SELECT id, name, kind FROM drives WHERE kind <> 'personal' AND disabled = 0 ORDER BY kind = 'team', name").fetch_all(&mut *conn).await?;
    let mut spaces: Vec<Space> = drives.into_iter().map(|d| Space { id: d.id, name: d.name, kind: d.kind, members: vec![], folders: vec![] }).collect();
    let mut folder_ids: Vec<String> = by_node.keys().cloned().collect();
    folder_ids.extend(places.nodes.values().filter(|n| n.restricted).map(|n| n.id.clone()));
    folder_ids.sort();
    folder_ids.dedup();
    for id in &folder_ids {
        let Some(n) = places.nodes.get(id).cloned() else { continue };
        let Some(space) = spaces.iter_mut().position(|s| s.id == n.drive_id) else { continue };
        let node_grants: Vec<Grant> = by_node.get(id).map(|gs| gs.iter().map(|g| grant_of(g)).collect()).unwrap_or_default();
        if n.at_root {
            spaces[space].members = node_grants;
        } else {
            let Some(place) = places.of(conn, id).await? else { continue };
            spaces[space].folders.push(Folder { place, restricted: n.restricted, grants: node_grants });
        }
    }
    for s in &mut spaces {
        s.folders.sort_by(|a, b| a.place.path.cmp(&b.place.path));
    }
    Ok(spaces)
}

/// Personal spaces: numbers only, by whose they are
fn personal_sharing(places: &Places, by_node: &ByNode<'_>, shares: &[ShareRow]) -> Vec<PersonalSharing> {
    let mut personal: BTreeMap<String, PersonalSharing> = BTreeMap::new();
    for (id, gs) in by_node {
        let n = &places.nodes[id];
        if n.drive_kind == "personal" && !n.at_root {
            let owner = n.owner_name.clone().unwrap_or_default();
            let p = personal.entry(owner.clone()).or_insert(PersonalSharing { owner, items: 0, grants: 0, links: 0 });
            p.items += 1;
            p.grants += gs.len() as i64;
        }
    }
    for s in shares {
        if let Some(n) = places.nodes.get(&s.node_id)
            && n.drive_kind == "personal"
        {
            let owner = n.owner_name.clone().unwrap_or_default();
            personal.entry(owner.clone()).or_insert(PersonalSharing { owner, items: 0, grants: 0, links: 0 }).links += 1;
        }
    }
    personal.into_values().collect()
}

/// Grants with an end date, soonest first
async fn expiring(conn: &mut SqliteConnection, places: &mut Places, live: &[&GrantRow]) -> AppResult<Vec<Expiring>> {
    let mut dated: Vec<&&GrantRow> = live.iter().filter(|g| g.expires_at.is_some()).collect();
    dated.sort_by_key(|g| g.expires_at);
    let mut out = vec![];
    for g in dated {
        if let Some(place) = places.of(conn, &g.node_id).await? {
            out.push(Expiring { place, grant: grant_of(g) });
        }
    }
    Ok(out)
}

/// Each account, and where the grants that name it (directly, through a group or as everyone) reach
async fn people(conn: &mut SqliteConnection, places: &mut Places, live: &[&GrantRow], t: i64) -> AppResult<Vec<Person>> {
    let users: Vec<UserRow> =
        sqlx::query_as("SELECT id, username, display_name, guest, role, disabled FROM users ORDER BY username COLLATE NOCASE").fetch_all(&mut *conn).await?;
    let memberships: Vec<(i64, i64, String)> =
        sqlx::query_as("SELECT m.user_id, g.id, g.name FROM group_members m JOIN groups g ON g.id = m.group_id ORDER BY g.name COLLATE NOCASE")
            .fetch_all(&mut *conn)
            .await?;
    let app_passwords: Vec<AppPasswordRow> = sqlx::query_as(
        "SELECT user_id, name, scope, created_at, expires_at, last_used_at FROM app_passwords WHERE expires_at IS NULL OR expires_at > ? ORDER BY created_at",
    )
    .bind(t)
    .fetch_all(&mut *conn)
    .await?;
    let ssh_keys: Vec<SshKeyRow> = sqlx::query_as("SELECT user_id, name, created_at, last_used_at FROM sftp_keys ORDER BY created_at").fetch_all(&mut *conn).await?;
    let mut people = vec![];
    for u in users {
        let groups: Vec<(i64, String)> = memberships.iter().filter(|(user, _, _)| *user == u.id).map(|(_, g, name)| (*g, name.clone())).collect();
        let mut reach = vec![];
        for g in live {
            let via = match g.principal_type.as_str() {
                "user" if g.principal_id == u.id => Some(("direct", None)),
                "group" => groups.iter().find(|(id, _)| *id == g.principal_id).map(|(_, name)| ("group", Some(name.clone()))),
                "everyone" if !u.guest => Some(("everyone", None)),
                _ => None,
            };
            let Some((via, group)) = via else { continue };
            if u.guest && !reaches_guest(g) {
                continue;
            }
            if let Some(place) = places.of(conn, &g.node_id).await? {
                reach.push(Reach { place, role: g.role.clone(), via: via.into(), group, expires_at: g.expires_at });
            }
        }
        people.push(Person {
            id: u.id,
            username: u.username,
            display_name: u.display_name,
            guest: u.guest,
            admin: u.role == "admin",
            disabled: u.disabled,
            groups: groups.into_iter().map(|(_, name)| name).collect(),
            reach,
            app_passwords: app_passwords.iter().filter(|p| p.user_id == u.id).map(|p| p.password.clone()).collect(),
            ssh_keys: ssh_keys.iter().filter(|k| k.user_id == u.id).map(|k| k.key.clone()).collect(),
        });
    }
    Ok(people)
}

async fn links(conn: &mut SqliteConnection, places: &mut Places, shares: Vec<ShareRow>) -> AppResult<Vec<Link>> {
    let mut links = vec![];
    for s in shares {
        if let Some(place) = places.of(conn, &s.node_id).await? {
            links.push(Link {
                place,
                creator: s.creator,
                created_at: s.created_at,
                expires_at: s.expires_at,
                password: s.password,
                allow_upload: s.allow_upload,
                allow_download: s.allow_download,
                downloads: s.downloads,
                max_downloads: s.max_downloads,
                views: s.views,
                last_access: s.last_access,
            });
        }
    }
    Ok(links)
}

async fn buckets(conn: &mut SqliteConnection, places: &mut Places, rows: Vec<BucketRow>, t: i64) -> AppResult<Vec<Bucket>> {
    let mut buckets = vec![];
    for b in rows {
        let Some(place) = places.of(conn, &b.node_id).await? else { continue };
        let keys: Vec<BucketKey> = sqlx::query_as(
            "SELECT description, writable, enabled, expires_at, last_used_at FROM s3_keys WHERE bucket_id = ? AND (expires_at IS NULL OR expires_at > ?) ORDER BY created_at",
        )
        .bind(&b.id)
        .bind(t)
        .fetch_all(&mut *conn)
        .await?;
        buckets.push(Bucket { name: b.name, place, creator: b.creator, enabled: b.enabled, keys });
    }
    Ok(buckets)
}

async fn receive_folders(conn: &mut SqliteConnection, places: &mut Places, rows: Vec<FtpRow>) -> AppResult<Vec<ReceiveFolder>> {
    let mut folders = vec![];
    for f in rows {
        if let Some(place) = places.of(conn, &f.node_id).await? {
            folders.push(ReceiveFolder {
                name: f.name,
                username: f.username,
                place,
                creator: f.creator,
                enabled: f.enabled,
                allow_list: f.allow_list,
                allow_read: f.allow_read,
                expires_at: f.expires_at,
                last_used_at: f.last_used_at,
            });
        }
    }
    Ok(folders)
}

pub async fn overview(conn: &mut SqliteConnection) -> AppResult<Overview> {
    let t = now();
    let grants = load_grants(conn, t).await?;
    let outside = load_outside(conn, t).await?;
    let ids = mentioned(conn, &grants, &outside).await?;
    let mut places = Places { nodes: nodes(conn, ids).await?, paths: HashMap::new() };
    // The grants of items that are there (not in the trash, in spaces that are on)
    let live: Vec<&GrantRow> = grants.iter().filter(|g| places.nodes.contains_key(&g.node_id)).collect();
    let mut by_node: ByNode = BTreeMap::new();
    for g in &live {
        by_node.entry(g.node_id.clone()).or_default().push(g);
    }
    let r: Reminder = db::json_setting(conn, SETTING).await?;
    Ok(Overview {
        generated_at: t,
        spaces: spaces(conn, &mut places, &by_node).await?,
        personal: personal_sharing(&places, &by_node, &outside.shares),
        expiring: expiring(conn, &mut places, &live).await?,
        people: people(conn, &mut places, &live, t).await?,
        links: links(conn, &mut places, outside.shares).await?,
        buckets: buckets(conn, &mut places, outside.buckets, t).await?,
        receive_folders: receive_folders(conn, &mut places, outside.ftp).await?,
        review: ReviewState { every_days: r.every_days, due_at: r.due_at(), last_reviewed_at: r.last_reviewed_at, last_reviewed_by: r.last_reviewed_by },
    })
}

pub async fn get(State(st): State<AppState>, _: Admin) -> AppResult<Json<Overview>> {
    Ok(Json(overview(&mut *st.db.acquire().await?).await?))
}

// ───────────── The reminder ─────────────

#[derive(Deserialize)]
pub struct ReminderChange {
    pub every_days: u32,
}

pub async fn set_reminder(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<ReminderChange>) -> AppResult<Json<ReviewState>> {
    if req.every_days > MAX_DAYS {
        return Err(AppError::bad_request(format!("Ask for a review at most every {MAX_DAYS} days")));
    }
    let _w = st.write_lock.lock().await;
    let mut tx = db::begin_write(&st.db).await?;
    let user = crate::auth::require_admin(&mut tx, &user).await?;
    let mut r: Reminder = db::json_setting(&mut tx, SETTING).await?;
    if req.every_days > 0 && r.every_days == 0 {
        r.since = Some(now());
    }
    r.every_days = req.every_days;
    db::set_json_setting(&mut tx, SETTING, &r).await?;
    let detail = if r.every_days > 0 { format!("Access reviews are asked for every {} days", r.every_days) } else { "Access reviews aren't asked for".into() };
    crate::logs::record_activity(&mut tx, &user, None, "settings", &detail).await?;
    tx.commit().await?;
    Ok(Json(ReviewState { every_days: r.every_days, due_at: r.due_at(), last_reviewed_at: r.last_reviewed_at, last_reviewed_by: r.last_reviewed_by }))
}

/// "Mark as reviewed": in the activity log, and the next review counted from now
pub async fn reviewed(State(st): State<AppState>, Admin(user): Admin) -> AppResult<Json<ReviewState>> {
    let _w = st.write_lock.lock().await;
    let mut tx = db::begin_write(&st.db).await?;
    let user = crate::auth::require_admin(&mut tx, &user).await?;
    let mut r: Reminder = db::json_setting(&mut tx, SETTING).await?;
    r.last_reviewed_at = Some(now());
    r.last_reviewed_by = Some(user.username.clone());
    // Told again only when the next one is due; no "done" notice
    r.alerted = String::new();
    db::set_json_setting(&mut tx, SETTING, &r).await?;
    crate::logs::record_activity(&mut tx, &user, None, "access_review", "Reviewed access").await?;
    tx.commit().await?;
    Ok(Json(ReviewState { every_days: r.every_days, due_at: r.due_at(), last_reviewed_at: r.last_reviewed_at, last_reviewed_by: r.last_reviewed_by }))
}

/// Looks once an hour whether a review is due, and tells administrators once when it is
pub fn spawn_reminder(st: AppState) {
    crate::util::supervise("Access review reminder", move |_| {
        let st = st.clone();
        async move {
            loop {
                if let Err(e) = remind(&st, now()).await {
                    tracing::warn!("Access review reminder: {}", e.message);
                }
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        }
    });
}

pub async fn remind(st: &AppState, t: i64) -> AppResult<()> {
    let r: Reminder = db::json_setting(&mut *st.db.acquire().await?, SETTING).await?;
    let due = r.due_at().is_some_and(|d| d <= t);
    // Told once when it becomes due (`alert` says nothing while that is what was last told); marking it reviewed says
    // nothing either
    if !due {
        return Ok(());
    }
    let a = crate::backups::scheduler::Alert {
        state: "due",
        alerted: &r.alerted,
        record: "UPDATE settings SET value = json_set(value, '$.alerted', ?1) WHERE key = ?2",
        id: SETTING,
        notice: "access_review",
        data: json!({ "every_days": r.every_days, "last_reviewed_at": r.last_reviewed_at }),
        action: "access_review",
        name: "Access review",
    };
    crate::backups::scheduler::alert(st, a, t).await
}

#[cfg(test)]
mod tests;
