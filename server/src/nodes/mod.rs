//! Browsing and organizing files: listing, folders, rename, move, copy, trash, search, favorites, shared with me.
//! Permissions are determined by space and folder grants (see tree::role_on).

mod find;
mod list;
mod organize;
mod trash;

pub use find::*;
pub use list::*;
pub use organize::*;
pub use trash::*;

use std::collections::{HashMap, HashSet};

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    fsops,
    jobs::{self, Job, Limit, Outcome},
    logs, paths,
    state::AppState,
    tree::{
        self, Crumb, NODE_COLS, Need, Node, Role,
        changes::{self, NOT_PURGING},
    },
    util::{new_id, now, validate_name},
};

const MAX_BATCH: usize = 1000;
/// Files and folders one copy may create (counted after expanding folders)
const MAX_COPY_ITEMS: usize = 20_000;
/// First segment of the location text for items accessed through a folder share (the browser uses its own text)
const SHARED_WITH_ME: &str = "Shared with me";

/// What people who can open a space are told about it
#[derive(Serialize)]
pub struct DriveBrief {
    id: String,
    name: String,
    kind: tree::SpaceKind,
    root_id: String,
}

impl From<tree::Drive> for DriveBrief {
    fn from(d: tree::Drive) -> Self {
        DriveBrief { id: d.id, name: d.name, kind: d.kind, root_id: d.root_id }
    }
}

#[derive(Serialize)]
pub struct NodeInfo {
    node: Node,
    /// Path from the space root (exclusive) to this node; when accessed through a folder share, starts at the shared folder
    path: Vec<Crumb>,
    is_root: bool,
    drive: DriveBrief,
    role: Role,
    /// Accessed through a folder share (rather than as a space member)
    via_share: bool,
    /// The path that names this item for the user, as typed into the address bar or used over WebDAV (see paths.rs):
    /// `["My files", "Reports"]`; None when no path reaches it
    location: Option<Vec<String>>,
    /// Reason the storage location holding the content is offline (files: where the content is; folders: the space's location)
    offline: Option<String>,
    /// A read-only space: browse, download and share only
    read_only: bool,
    /// The space is being moved to another storage location, and is read-only until the move finishes
    moving: bool,
}

/// The path visible to the user: space members see the full path; people with shared access only see from the shared folder down
async fn visible_path(conn: &mut SqliteConnection, user: &User, node: &Node) -> AppResult<(Vec<Crumb>, bool)> {
    let path = tree::path_of(conn, &node.id).await?;
    let member_of = tree::member_of(conn, user).await?;
    if member_of.iter().any(|d| d == node.drive()) {
        return Ok((path, false));
    }
    let shared = tree::shared_ids(conn, user, &member_of).await?;
    let start = tree::shared_start(&path, &shared).unwrap_or(0);
    Ok((path.into_iter().skip(start).collect(), true))
}

pub async fn get(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<NodeInfo>> {
    let mut c = st.db.acquire().await?;
    let (mut node, role) = tree::node_with_role(&mut c, &user, &id).await?;
    let drive = tree::get_drive(&mut c, node.drive()).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    let (path, via_share) = visible_path(&mut c, &user, &node).await?;
    let location = paths::location_of(&mut c, &user, &node).await?;
    tree::mark_favorites(&mut c, user.id, [&mut node]).await?;
    let is_root = node.parent_id.is_none();
    // A folder space is offline with its location (a disk that may not be mounted); a folder an administrator chose
    // is on no location
    let offline = if drive.is_folder() {
        let (location,): (Option<String>,) = sqlx::query_as("SELECT location_id FROM drives WHERE id = ?").bind(&drive.id).fetch_one(&mut *c).await?;
        match location.and_then(|l| st.location_offline_for(&l, user.is_admin())) {
            // A file with a checked replica elsewhere still opens (replicas/)
            Some(_) if node.kind == "file" && crate::replicas::folders::copy_of(&st, &mut c, &node.id).await?.is_some() => None,
            offline => offline,
        }
    } else {
        match node.blob() {
            // A file whose content has a checked replica elsewhere still opens (replicas/)
            Ok((hash, loc)) => match st.location_offline_for(loc, user.is_admin()) {
                Some(_) if crate::replicas::readable_elsewhere(&st, &mut c, hash, loc).await? => None,
                offline => offline,
            },
            Err(_) => {
                let location = tree::drive_location(&mut c, node.drive()).await?;
                st.location_offline_for(&location, user.is_admin())
            }
        }
    };
    let (read_only, moving) = (drive.read_only || drive.moving, drive.moving);
    Ok(Json(NodeInfo {
        node,
        path,
        is_root,
        drive: drive.into(),
        role,
        via_share,
        location,
        offline,
        read_only,
        moving,
    }))
}

#[derive(Deserialize)]
pub struct ContentsReq {
    ids: Vec<String>,
}

/// What a set of folders holds, like Size and Contains in the Windows properties: everything inside them, at any depth
#[derive(Serialize, Debug, PartialEq)]
pub struct Contents {
    /// Bytes of the files inside
    size: i64,
    files: i64,
    folders: i64,
}

/// Size and number of items inside the given folders (the items themselves are not counted; a file holds nothing).
/// Items in the trash are left out. For the Details pane, of one folder or of a selection of several.
pub async fn contents(State(st): State<AppState>, user: User, Json(req): Json<ContentsReq>) -> AppResult<Json<Contents>> {
    let ids = BatchReq { ids: req.ids, dest_id: None, resolutions: Default::default() }.ids()?;
    let mut c = st.db.acquire().await?;
    let mut folders = Vec::with_capacity(ids.len());
    for id in &ids {
        let (node, _) = tree::node_with_role(&mut c, &user, id).await?;
        if node.is_folder() {
            folders.push(node.id);
        }
    }
    // A folder selected together with a folder around it would be counted twice
    let folders = outermost(&mut c, &folders).await?;
    // Only the columns needed, and only folders are expanded, so even a large space is summed quickly
    let (size, files, folders): (i64, i64, i64) = sqlx::query_as(
        "WITH RECURSIVE sub(id, kind, size) AS (
           SELECT n.id, n.kind, n.size FROM nodes n
           WHERE n.parent_id IN (SELECT value FROM json_each(?1)) AND n.trashed_at IS NULL
           UNION ALL
           SELECT n.id, n.kind, n.size FROM nodes n JOIN sub ON n.parent_id = sub.id
           WHERE sub.kind = 'folder' AND n.trashed_at IS NULL
         )
         SELECT COALESCE(SUM(CASE WHEN kind = 'file' THEN size ELSE 0 END), 0),
                COALESCE(SUM(kind = 'file'), 0), COALESCE(SUM(kind = 'folder'), 0) FROM sub",
    )
    .bind(serde_json::to_string(&folders).unwrap())
    .fetch_one(&mut *c)
    .await?;
    Ok(Json(Contents { size, files, folders }))
}

#[derive(Serialize)]
pub struct Located {
    #[serde(flatten)]
    node: Node,
    /// Location, e.g. "All files/Projects/2026" (English; the browser builds its own text from the parts below)
    location: String,
    /// The space the item is in, or None when it is only reached through something shared with the user
    location_space: Option<SpaceRef>,
    /// Folders from the space root (or the shared folder) down to the item's parent
    location_path: Vec<String>,
    /// Trash: who moved the item there; None when unknown (deleted by a removed account)
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted_by: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct SpaceRef {
    kind: tree::SpaceKind,
    name: String,
}

/// Attaches each node's location (space name + path) and marks favorites
async fn locate(st: &AppState, user: &User, mut nodes: Vec<Node>) -> AppResult<Vec<Located>> {
    let mut c = st.db.acquire().await?;
    tree::mark_favorites(&mut c, user.id, &mut nodes).await?;
    let mut drives: HashMap<String, SpaceRef> =
        tree::user_drives(&mut c, user).await?.into_iter().map(|(d, _)| (d.id, SpaceRef { kind: d.kind, name: d.name })).collect();
    if user.is_admin() && nodes.iter().any(|n| !drives.contains_key(n.drive())) {
        // Spaces an administrator manages without being a member (their trash is listed too)
        let all: Vec<(String, tree::SpaceKind, String)> =
            sqlx::query_as("SELECT id, kind, name FROM drives WHERE kind != 'personal' AND disabled = 0").fetch_all(&mut *c).await?;
        for (id, kind, name) in all {
            drives.entry(id).or_insert(SpaceRef { kind, name });
        }
    }
    let shared: HashSet<String> = if nodes.iter().any(|n| !drives.contains_key(n.drive())) {
        let member_of: Vec<String> = drives.keys().cloned().collect();
        tree::shared_ids(&mut c, user, &member_of).await?
    } else {
        HashSet::new()
    };
    // One query for the paths of all parents instead of one recursive query per row
    let parent_ids: Vec<String> = nodes.iter().filter_map(|n| n.parent_id.clone()).collect::<HashSet<_>>().into_iter().collect();
    let paths = tree::paths_of(&mut c, &parent_ids).await?;
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        let path = match &node.parent_id {
            Some(p) => paths.get(p).cloned().unwrap_or_default(),
            None => Vec::new(),
        };
        let space = drives.get(node.drive()).cloned();
        // Accessed through a folder share: only shown from the shared folder down
        let start = if space.is_some() { 0 } else { tree::shared_start(&path, &shared).unwrap_or(path.len()) };
        let location_path: Vec<String> = path[start..].iter().map(|p| p.name.clone()).collect();
        let first = space.as_ref().map_or(SHARED_WITH_ME, |s| s.name.as_str());
        let location = std::iter::once(first).chain(location_path.iter().map(String::as_str)).collect::<Vec<_>>().join("/");
        out.push(Located { node, location, location_space: space, location_path, deleted_by: None });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use axum::http::StatusCode;

    fn batch(ids: &[&str], dest: &str) -> Json<BatchReq> {
        Json(BatchReq { ids: ids.iter().map(|s| s.to_string()).collect(), dest_id: Some(dest.to_string()), ..Default::default() })
    }

    fn ids(list: &[&str]) -> Json<BatchReq> {
        Json(BatchReq { ids: list.iter().map(|s| s.to_string()).collect(), dest_id: None, ..Default::default() })
    }

    #[tokio::test]
    async fn names_differing_only_in_non_english_letter_case_are_the_same_name() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, amy.root(), "Été").await;
        let mut c = env.st.db.acquire().await.unwrap();
        assert!(tree::name_taken(&mut c, amy.root(), "été").await.unwrap());
        assert_eq!(tree::unique_name(&mut c, amy.root(), "ÉTÉ", true).await.unwrap(), "ÉTÉ (1)");
        // An uploaded folder "été/x" goes into the existing "Été"
        let found = tree::ensure_folders(&mut c, amy.id, amy.root(), "été", "").await.unwrap();
        assert_eq!(found, folder);
        // The database refuses a second one too
        let dup = sqlx::query(
            "INSERT INTO nodes (id, owner_id, parent_id, kind, name, drive_id, created_at, updated_at)
             SELECT 'x', owner_id, id, 'folder', 'été', drive_id, 0, 0 FROM nodes WHERE id = ?",
        )
        .bind(amy.root())
        .execute(&mut *c)
        .await;
        assert!(dup.is_err());
        drop(c);
        // Only its letter case changes: the same name, not one that is taken
        let file = env.file(&amy, amy.root(), "été.txt").await;
        let Json(renamed) = rename(State(env.st.clone()), amy.clone(), Path(file), Json(RenameReq { name: "Été.txt".into() })).await.unwrap();
        assert_eq!(renamed.name, "Été.txt");
    }

    #[tokio::test]
    async fn folders_list_names_in_natural_order_and_types_by_extension() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        for name in ["File 10.txt", "file 2.txt", "File 1.docx", "b.pdf", "README"] {
            env.file(&amy, amy.root(), name).await;
        }
        let list = |sort: &'static str| {
            let (st, amy) = (env.st.clone(), amy.clone());
            async move {
                let q = ListQuery { sort: Some(sort.into()), ..Default::default() };
                let items = children(State(st), amy.clone(), Path(amy.root().to_string()), Query(q)).await.unwrap().0.into_items();
                items.into_iter().map(|n| n.name).collect::<Vec<_>>()
            }
        };
        assert_eq!(list("name").await, ["b.pdf", "File 1.docx", "file 2.txt", "File 10.txt", "README"]);
        assert_eq!(list("type").await, ["README", "File 1.docx", "b.pdf", "file 2.txt", "File 10.txt"]);
    }

    /// Lists a folder page by page (`limit` items each) until the end
    async fn all_pages(env: &testutil::TestEnv, user: &User, folder: &str, sort: &str, order: &str, limit: i64) -> Vec<String> {
        let (mut names, mut after) = (Vec::new(), None);
        loop {
            let q = ListQuery { sort: Some(sort.into()), order: Some(order.into()), limit: Some(limit), after, ..Default::default() };
            let Json(page) = children(State(env.st.clone()), user.clone(), Path(folder.to_string()), Query(q)).await.unwrap();
            let Listing::Page { items, next, .. } = page else { panic!("a limit gives a page") };
            assert!(items.len() as i64 <= limit);
            names.extend(items.into_iter().map(|n| n.name));
            match next {
                Some(n) => after = Some(n),
                None => return names,
            }
        }
    }

    #[tokio::test]
    async fn folders_list_page_by_page_in_every_sort_order() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        // Equal sizes, times and extensions, so the ties are ordered by name and id across page boundaries
        mixed_folder(&env, &amy).await;
        for sort in ["name", "size", "updated", "created", "type"] {
            for order in ["asc", "desc"] {
                let q = ListQuery { sort: Some(sort.into()), order: Some(order.into()), ..Default::default() };
                let Json(whole) = children(State(env.st.clone()), amy.clone(), Path(amy.root().to_string()), Query(q)).await.unwrap();
                let Listing::All(whole) = whole else { panic!("no limit gives the whole list") };
                let whole: Vec<String> = whole.into_iter().map(|n| n.name).collect();
                assert_eq!(whole.len(), 15);
                assert!(whole[..4].iter().all(|n| !n.contains('.') && n != "README" && n != "e"), "folders first: {whole:?}");
                for limit in [1, 2, 4, 15, 100] {
                    assert_eq!(all_pages(&env, &amy, amy.root(), sort, order, limit).await, whole, "{sort} {order}, {limit} per page");
                }
            }
        }
    }

    #[tokio::test]
    async fn the_next_page_starts_after_the_last_item_even_when_the_folder_changed() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        for i in 1..=6 {
            env.file(&amy, amy.root(), &format!("{i}.txt")).await;
        }
        let page = |after: Option<String>| {
            let (st, amy) = (env.st.clone(), amy.clone());
            async move {
                let q = ListQuery { limit: Some(3), after, ..Default::default() };
                match children(State(st), amy.clone(), Path(amy.root().to_string()), Query(q)).await.unwrap().0 {
                    Listing::Page { items, next, .. } => (items.into_iter().map(|n| n.name).collect::<Vec<_>>(), next),
                    Listing::All(_) => panic!("a limit gives a page"),
                }
            }
        };
        let (first, next) = page(None).await;
        assert_eq!(first, ["1.txt", "2.txt", "3.txt"]);
        // The last item of the page is renamed away and one is added before it: nothing is repeated or skipped
        let (three,): (String,) = sqlx::query_as("SELECT id FROM nodes WHERE name = '3.txt'").fetch_one(&env.st.db).await.unwrap();
        let _ = rename(State(env.st.clone()), amy.clone(), Path(three), Json(RenameReq { name: "9.txt".into() })).await.unwrap();
        env.file(&amy, amy.root(), "0.txt").await;
        let (second, next) = page(next).await;
        assert_eq!(second, ["4.txt", "5.txt", "6.txt"]);
        let (third, next) = page(next).await;
        assert_eq!((third, next), (vec!["9.txt".to_string()], None));

        // A cursor that wasn't made by the server
        let q = ListQuery { limit: Some(3), after: Some("not a cursor".into()), ..Default::default() };
        let err = children(State(env.st.clone()), amy.clone(), Path(amy.root().to_string()), Query(q)).await.unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    /// A folder of 4 folders and 11 files whose sizes, times and extensions tie, so ties are ordered by name and id
    async fn mixed_folder(env: &testutil::TestEnv, amy: &User) {
        for name in ["Zeta", "alpha", "Folder 10", "Folder 9"] {
            env.folder(amy, amy.root(), name).await;
        }
        for (i, name) in ["File 10.txt", "file 2.txt", "File 1.docx", "b.pdf", "README", "c.TXT", "d.pdf", "e", "a.docx", "f.txt", "g.png"].iter().enumerate() {
            let id = env.file(amy, amy.root(), name).await;
            sqlx::query("UPDATE nodes SET size = ?, updated_at = ?, created_at = ? WHERE id = ?")
                .bind((i % 3) as i64 * 100)
                .bind(1_000 + (i % 4) as i64)
                .bind(500 + (i % 2) as i64)
                .bind(&id)
                .execute(&env.st.db)
                .await
                .unwrap();
        }
    }

    /// The whole listing of amy's root folder in one order: (id, name)
    async fn whole(env: &testutil::TestEnv, amy: &User, sort: &str, order: &str) -> Vec<(String, String)> {
        let q = ListQuery { sort: Some(sort.into()), order: Some(order.into()), ..Default::default() };
        let Json(Listing::All(all)) = children(State(env.st.clone()), amy.clone(), Path(amy.root().to_string()), Query(q)).await.unwrap() else {
            panic!("no limit gives the whole list")
        };
        all.into_iter().map(|n| (n.id, n.name)).collect()
    }

    #[tokio::test]
    async fn any_part_of_a_folder_can_be_listed_by_its_position_with_the_number_of_items() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        mixed_folder(&env, &amy).await;
        for sort in ["name", "size", "updated", "created", "type"] {
            for order in ["asc", "desc"] {
                let all: Vec<String> = whole(&env, &amy, sort, order).await.into_iter().map(|(_, n)| n).collect();
                for limit in [1, 4, 15, 20] {
                    let mut names = Vec::new();
                    // Parts asked for in any order give the same list
                    let offsets: Vec<i64> = (0..20).step_by(limit as usize).collect();
                    for offset in offsets.into_iter().rev() {
                        let q = ListQuery { sort: Some(sort.into()), order: Some(order.into()), limit: Some(limit), offset: Some(offset), ..Default::default() };
                        let Json(Listing::Page { items, total, .. }) = children(State(env.st.clone()), amy.clone(), Path(amy.root().to_string()), Query(q)).await.unwrap() else {
                            panic!("a limit gives a page")
                        };
                        assert_eq!(total, Some(15));
                        assert!(items.len() as i64 <= limit);
                        names.splice(0..0, items.into_iter().map(|n| n.name));
                    }
                    assert_eq!(names, all, "{sort} {order}, {limit} per page");
                }
            }
        }
        // Without a position, pages don't count the items (as before)
        let q = ListQuery { limit: Some(2), ..Default::default() };
        let Json(page) = children(State(env.st.clone()), amy.clone(), Path(amy.root().to_string()), Query(q)).await.unwrap();
        assert!(serde_json::to_value(&page).unwrap().get("total").is_none());
    }

    #[tokio::test]
    async fn the_position_of_an_item_is_where_the_listing_shows_it() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        mixed_folder(&env, &amy).await;
        for sort in ["name", "size", "updated", "created", "type"] {
            for order in ["asc", "desc"] {
                for (i, (id, _)) in whole(&env, &amy, sort, order).await.into_iter().enumerate() {
                    let q = PositionQuery { item: id, sort: Some(sort.into()), order: Some(order.into()) };
                    let Json(p) = position(State(env.st.clone()), amy.clone(), Path(amy.root().to_string()), Query(q)).await.unwrap();
                    assert_eq!((p.position, p.total), (Some(i as i64), 15), "{sort} {order}");
                }
            }
        }
        // Not in the folder
        let other = env.folder(&amy, amy.root(), "Other").await;
        let inside = env.file(&amy, &other, "inside.txt").await;
        let q = PositionQuery { item: inside, sort: None, order: None };
        let Json(p) = position(State(env.st.clone()), amy.clone(), Path(amy.root().to_string()), Query(q)).await.unwrap();
        assert_eq!((p.position, p.total), (None, 16));
        // Someone who can't open the folder isn't told anything
        let bob = env.user("bob", true).await;
        let q = PositionQuery { item: other.clone(), sort: None, order: None };
        assert!(position(State(env.st.clone()), bob, Path(amy.root().to_string()), Query(q)).await.is_err());
    }

    #[tokio::test]
    async fn a_selection_in_a_large_folder_comes_a_batch_at_a_time_and_is_the_same_while_it_is_changed() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        mixed_folder(&env, &amy).await;
        let select_all = |req: SelectReq, trash_each: bool| {
            let (st, amy) = (env.st.clone(), amy.clone());
            async move {
                let (mut out, mut after) = (Vec::new(), None);
                loop {
                    let r = SelectReq { after: after.clone(), limit: Some(4), ..req.clone() };
                    let Json(page) = select(State(st.clone()), amy.clone(), Path(amy.root().to_string()), Json(r)).await.unwrap();
                    assert!(page.ids.len() <= 4);
                    // What the browser does with each batch (here: to the trash) doesn't change the next one
                    if trash_each && !page.ids.is_empty() {
                        let req = BatchReq { ids: page.ids.clone(), ..Default::default() };
                        let _ = trash(State(st.clone()), amy.clone(), Json(req)).await.unwrap();
                    }
                    out.extend(page.ids);
                    match page.next {
                        Some(n) => after = Some(n),
                        None => return out,
                    }
                }
            }
        };
        for sort in ["name", "size", "type"] {
            for order in ["asc", "desc"] {
                let all: Vec<String> = whole(&env, &amy, sort, order).await.into_iter().map(|(id, _)| id).collect();
                let base = SelectReq { sort: Some(sort.into()), order: Some(order.into()), from: None, to: None, except: vec![], after: None, limit: None };
                // Everything, less what was left out
                let except = vec![all[1].clone(), all[7].clone()];
                let got = select_all(SelectReq { except: except.clone(), ..base.clone() }, false).await;
                assert_eq!(got, all.iter().filter(|id| !except.contains(id)).cloned().collect::<Vec<_>>(), "{sort} {order}");
                // From one item to another, both included
                let got = select_all(SelectReq { from: Some(all[3].clone()), to: Some(all[12].clone()), ..base.clone() }, false).await;
                assert_eq!(got, all[3..=12].to_vec(), "{sort} {order}");
                let got = select_all(SelectReq { from: Some(all[13].clone()), ..base.clone() }, false).await;
                assert_eq!(got, all[13..].to_vec());
                let got = select_all(SelectReq { to: Some(all[2].clone()), ..base.clone() }, false).await;
                assert_eq!(got, all[..=2].to_vec());
            }
        }
        // Batches put in the trash one after the other: every item is reached once
        let all: Vec<String> = whole(&env, &amy, "name", "asc").await.into_iter().map(|(id, _)| id).collect();
        let base = SelectReq { sort: None, order: None, from: Some(all[2].clone()), to: None, except: vec![all[5].clone()], after: None, limit: None };
        let got = select_all(base, true).await;
        let mut expected = all[2..].to_vec();
        expected.retain(|id| *id != all[5]);
        assert_eq!(got, expected);
        let left: Vec<String> = whole(&env, &amy, "name", "asc").await.into_iter().map(|(id, _)| id).collect();
        assert_eq!(left, vec![all[0].clone(), all[1].clone(), all[5].clone()]);

        // A first item no longer in the folder, and someone who can't open it
        let req = SelectReq { sort: None, order: None, from: Some(all[3].clone()), to: None, except: vec![], after: None, limit: None };
        let err = select(State(env.st.clone()), amy.clone(), Path(amy.root().to_string()), Json(req)).await.unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        let bob = env.user("bob", true).await;
        let req = SelectReq { sort: None, order: None, from: None, to: None, except: vec![], after: None, limit: None };
        assert!(select(State(env.st.clone()), bob, Path(amy.root().to_string()), Json(req)).await.is_err());
    }

    #[tokio::test]
    async fn a_listing_without_a_limit_is_a_plain_array() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        env.file(&amy, amy.root(), "a.txt").await;
        let Json(whole) = children(State(env.st.clone()), amy.clone(), Path(amy.root().to_string()), Query(ListQuery::default())).await.unwrap();
        let v = serde_json::to_value(&whole).unwrap();
        assert_eq!((v.as_array().map(Vec::len), v[0]["name"].as_str(), v[0]["owner_name"].as_str()), (Some(1), Some("a.txt"), Some("amy")));
        let q = ListQuery { limit: Some(10), ..Default::default() };
        let Json(page) = children(State(env.st.clone()), amy.clone(), Path(amy.root().to_string()), Query(q)).await.unwrap();
        let v = serde_json::to_value(&page).unwrap();
        assert_eq!((v["items"][0]["name"].as_str(), v["next"].is_null()), (Some("a.txt"), true));
    }

    #[tokio::test]
    async fn folder_listings_say_which_folders_have_folders_in_them() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let parent = env.folder(&amy, amy.root(), "Parent").await;
        env.folder(&amy, &parent, "Child").await;
        let files_only = env.folder(&amy, amy.root(), "Files only").await;
        env.file(&amy, &files_only, "a.txt").await;
        // A folder whose only folder is in the trash has none to show
        let emptied = env.folder(&amy, amy.root(), "Emptied").await;
        let gone = env.folder(&amy, &emptied, "Gone").await;
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&gone])).await.unwrap();
        let q = ListQuery { folders_only: Some(true), ..Default::default() };
        let Json(list) = children(State(env.st.clone()), amy.clone(), Path(amy.root().to_string()), Query(q)).await.unwrap();
        let v = serde_json::to_value(&list).unwrap();
        let flags: Vec<_> = v.as_array().unwrap().iter().map(|n| (n["name"].as_str().unwrap(), n["has_folders"].as_bool())).collect();
        assert_eq!(flags, [("Emptied", Some(false)), ("Files only", Some(false)), ("Parent", Some(true))]);
        // Other listings don't carry it
        let Json(all) = children(State(env.st.clone()), amy.clone(), Path(amy.root().to_string()), Query(ListQuery::default())).await.unwrap();
        assert!(serde_json::to_value(&all).unwrap()[0].get("has_folders").is_none());
    }

    #[tokio::test]
    async fn the_trash_lists_page_by_page_newest_first() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let mut files = Vec::new();
        for i in 0..7 {
            files.push(env.file(&amy, amy.root(), &format!("{i}.txt")).await);
        }
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&refs)).await.unwrap();
        // Deleted at different times, two of them at the same second
        for (i, id) in files.iter().enumerate() {
            sqlx::query("UPDATE nodes SET trashed_at = ? WHERE id = ?").bind(100 + (i as i64).min(5)).bind(id).execute(&env.st.db).await.unwrap();
        }
        let whole: Vec<String> =
            list_trash(State(env.st.clone()), amy.clone(), Query(TrashQuery::default())).await.unwrap().0.into_items().into_iter().map(|l| l.node.name).collect();
        assert_eq!(whole.len(), 7);
        assert_eq!(&whole[2..], ["4.txt", "3.txt", "2.txt", "1.txt", "0.txt"]);
        for limit in [1, 2, 3, 7] {
            let (mut names, mut after) = (Vec::new(), None);
            loop {
                let q = TrashQuery { limit: Some(limit), after, mine: None };
                let Json(Listing::Page { items, next, .. }) = list_trash(State(env.st.clone()), amy.clone(), Query(q)).await.unwrap() else {
                    panic!("a limit gives a page")
                };
                names.extend(items.into_iter().map(|l| l.node.name));
                let Some(n) = next else { break };
                after = Some(n);
            }
            assert_eq!(names, whole, "{limit} per page");
        }
    }

    #[tokio::test]
    async fn empty_trash_counts_and_deletes_only_what_the_user_manages() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let a = env.file(&amy, amy.root(), "a.txt").await;
        let b = env.file(&amy, amy.root(), "b.txt").await;
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&a, &b])).await.unwrap();
        let Json(preview) = empty_trash_preview(State(env.st.clone()), amy.clone()).await.unwrap();
        assert_eq!(preview.len(), 1);
        assert_eq!((preview[0].kind.as_str(), preview[0].items), ("personal", 2));

        // Without permission to delete, nothing is emptied and nothing is counted
        let mut bob = env.user("bob", false).await;
        let c = env.file(&bob, bob.root(), "c.txt").await;
        let _ = trash(State(env.st.clone()), bob.clone(), ids(&[&c])).await.unwrap();
        bob.can_delete = false;
        let Json(preview) = empty_trash_preview(State(env.st.clone()), bob.clone()).await.unwrap();
        assert!(preview.is_empty());

        let Json(done) = empty_trash(State(env.st.clone()), amy.clone()).await.unwrap();
        assert_eq!(done.result.unwrap()["deleted"], 2);
        let left = list_trash(State(env.st.clone()), bob.clone(), Query(TrashQuery::default())).await.unwrap().0.into_items();
        assert_eq!(left.len(), 1);
    }

    #[tokio::test]
    async fn copies_and_deleted_spaces_keep_content_references_right() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let team_root = {
            let mut conn = env.st.db.acquire().await.unwrap();
            let (_, root) = crate::db::create_drive(&mut conn, "Team", "team", amy.id, 0, "local").await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", amy.id, "owner", Some(amy.id), None).await.unwrap();
            root
        };
        // Two files with the same content, one level down
        let folder = env.folder(&amy, &team_root, "Docs").await;
        let (a, b) = (env.file(&amy, &folder, "a.txt").await, env.file(&amy, &folder, "b.txt").await);
        let hash = "ab".repeat(32);
        {
            let mut c = env.st.db.acquire().await.unwrap();
            for id in [&a, &b] {
                tree::add_blob_ref(&mut c, &hash, 5, "local").await.unwrap();
                sqlx::query("UPDATE nodes SET blob_hash = ?, size = 5 WHERE id = ?").bind(&hash).bind(id).execute(&mut *c).await.unwrap();
            }
        }
        let refs = || async {
            sqlx::query_as::<_, (i64,)>("SELECT refcount FROM blobs WHERE hash = ?").bind(&hash).fetch_optional(&env.st.db).await.unwrap().map(|r| r.0)
        };
        // Copying the folder adds a reference per file, in one statement
        let _ = copy_nodes(State(env.st.clone()), amy.clone(), batch(&[&folder], amy.root())).await.unwrap();
        assert_eq!(refs().await, Some(4));

        // Deleting the space removes it at once and its content in the background
        let team = env.drive_of(&team_root).await;
        let _ = crate::drives::delete(State(env.st.clone()), amy.clone(), Path(team.clone())).await.unwrap();
        for _ in 0..100 {
            let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = ?").bind(&team).fetch_one(&env.st.db).await.unwrap();
            if left == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = ?").bind(&team).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(left, 0);
        assert_eq!(refs().await, Some(2), "the copies still use the content");
    }

    #[tokio::test]
    async fn search_finds_names_in_any_letter_case_and_filters() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let reports = env.folder(&amy, amy.root(), "Reports").await;
        env.file(&amy, &reports, "Été 2026.xlsx").await;
        env.file(&amy, &reports, "budget.docx").await;
        env.file(&amy, amy.root(), "ete-notes.txt").await;
        env.file(&amy, amy.root(), "AB.txt").await;
        let search = |q: &str, within: Option<&str>, ext: Option<&str>| {
            let (st, amy) = (env.st.clone(), amy.clone());
            let q = SearchQuery {
                q: q.into(),
                within: within.map(str::to_string),
                kind: None,
                ext: ext.map(str::to_string),
                from: None,
                to: None,
                min_size: None,
                max_size: None,
                owner: None,
            };
            async move {
                let Json(r) = search(State(st), amy, Query(q)).await.unwrap();
                let mut names: Vec<String> = r.items.into_iter().map(|l| l.node.name).collect();
                names.sort();
                names
            }
        };
        // Letter case and accents in any language, through the trigram index
        assert_eq!(search("ÉTÉ", None, None).await, ["ete-notes.txt", "Été 2026.xlsx"]);
        // Short terms scan, also ignoring case
        assert_eq!(search("ab", None, None).await, ["AB.txt"]);
        // Only below a folder, and by type
        assert_eq!(search("ete", Some(&reports), None).await, ["Été 2026.xlsx"]);
        assert_eq!(search("e", None, Some("docx")).await, ["budget.docx"]);
        // Renames are indexed
        let id = env.file(&amy, amy.root(), "old name.txt").await;
        let _ = rename(State(env.st.clone()), amy.clone(), Path(id), Json(RenameReq { name: "Quarterly.txt".into() })).await.unwrap();
        assert_eq!(search("quarter", None, None).await, ["Quarterly.txt"]);
        assert!(search("old name", None, None).await.is_empty());
    }

    #[tokio::test]
    async fn the_same_item_selected_twice_is_copied_once() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let doc = env.file(&amy, amy.root(), "a.txt").await;
        let dest = env.folder(&amy, amy.root(), "Copies").await;
        let _ = copy_nodes(State(env.st.clone()), amy.clone(), batch(&[&doc, &doc, &doc], &dest)).await.unwrap();
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE parent_id = ?").bind(&dest).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn a_file_version_never_goes_back() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let doc = env.file(&amy, amy.root(), "a.txt").await;
        // Saved several times within a second: the version is ahead of the clock
        let ahead = now() + 5;
        sqlx::query("UPDATE nodes SET updated_at = ? WHERE id = ?").bind(ahead).bind(&doc).execute(&env.st.db).await.unwrap();
        let _ = rename(State(env.st.clone()), amy.clone(), Path(doc.clone()), Json(RenameReq { name: "b.txt".into() })).await.unwrap();
        let (after,): (i64,) = sqlx::query_as("SELECT updated_at FROM nodes WHERE id = ?").bind(&doc).fetch_one(&env.st.db).await.unwrap();
        assert!(after > ahead, "renaming moved the version back from {ahead} to {after}");
    }

    #[tokio::test]
    async fn a_selection_with_a_folder_and_something_inside_it_can_be_trashed_and_deleted() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, amy.root(), "Folder").await;
        let inner = env.folder(&amy, &folder, "Inner").await;
        let doc = env.file(&amy, &inner, "a.txt").await;
        let other = env.file(&amy, amy.root(), "b.txt").await;

        // Moving to the trash: the folder, a file deep inside it, and a duplicate id
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&doc, &folder, &other, &folder])).await.unwrap();
        let listed = list_trash(State(env.st.clone()), amy.clone(), Query(TrashQuery::default())).await.unwrap().0.into_items();
        let mut names: Vec<String> = listed.into_iter().map(|l| l.node.name).collect();
        names.sort();
        assert_eq!(names, vec!["Folder", "b.txt"]);

        // Deleting for good: an item trashed on its own before its folder is listed separately; selecting both works
        let _ = restore(State(env.st.clone()), amy.clone(), ids(&[&folder])).await.unwrap();
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&doc])).await.unwrap();
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&folder])).await.unwrap();
        let listed = list_trash(State(env.st.clone()), amy.clone(), Query(TrashQuery::default())).await.unwrap().0.into_items();
        let all: Vec<String> = listed.into_iter().map(|l| l.node.id).collect();
        assert!(all.contains(&doc) && all.contains(&folder));
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        let _ = delete_forever(State(env.st.clone()), amy.clone(), ids(&refs)).await.unwrap();
        let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE id IN (?, ?, ?, ?)")
            .bind(&folder)
            .bind(&inner)
            .bind(&doc)
            .bind(&other)
            .fetch_one(&env.st.db)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn many_listings_at_once_share_the_connection_pool() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, amy.root(), "docs").await;
        let doc = env.file(&amy, &folder, "a.txt").await;
        // Each listing used to hold one connection while waiting for a second one, so more listings than
        // connections at the same moment waited for each other until the pool timed out
        let listings = (0..32).map(|i| {
            let (st, amy, folder, doc) = (env.st.clone(), amy.clone(), folder.clone(), doc.clone());
            async move {
                if i % 2 == 0 {
                    let q = Query(ListQuery::default());
                    children(State(st), amy, Path(folder), q).await.map(|_| ())
                } else {
                    get(State(st), amy, Path(doc)).await.map(|_| ())
                }
            }
        });
        let all = futures_util::future::join_all(listings.map(tokio::spawn));
        let results = tokio::time::timeout(std::time::Duration::from_secs(10), all).await.expect("listings stalled");
        assert!(results.into_iter().all(|r| r.unwrap().is_ok()));
    }

    #[tokio::test]
    async fn folder_share_editor_can_only_restore_or_destroy_items_inside_the_share() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let shared = env.folder(&amy, amy.root(), "shared").await;
        let doc = env.file(&amy, &shared, "report.txt").await;
        env.grant(&shared, &ben, "editor").await;

        // Ben trashes a file inside the shared folder: he may restore it (the folder is his to edit)
        let _ = trash(State(env.st.clone()), ben.clone(), ids(&[&doc])).await.unwrap();
        let _ = restore(State(env.st.clone()), ben.clone(), ids(&[&doc])).await.unwrap();

        // Amy trashes the shared folder itself: Ben has no rights on Amy's root folder, so he can neither restore nor destroy it
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&shared])).await.unwrap();
        let err = restore(State(env.st.clone()), ben.clone(), ids(&[&shared])).await.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        let err = delete_forever(State(env.st.clone()), ben.clone(), ids(&[&shared])).await.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        // The owner can
        let _ = restore(State(env.st.clone()), amy.clone(), ids(&[&shared])).await.unwrap();
        assert_eq!(env.drive_of(&doc).await, env.drive_of(amy.root()).await);
    }

    #[tokio::test]
    async fn administrators_manage_the_trash_of_team_spaces_they_are_not_members_of() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let admin = env.admin().await;
        let (team_root, doc, other, private) = {
            let mut conn = env.st.db.acquire().await.unwrap();
            let (_, root) = crate::db::create_drive(&mut conn, "Team", "team", amy.id, 0, "local").await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", amy.id, "owner", Some(amy.id), None).await.unwrap();
            drop(conn);
            let doc = env.file(&amy, &root, "plan.txt").await;
            let other = env.file(&amy, &root, "old.txt").await;
            let private = env.file(&amy, amy.root(), "diary.txt").await;
            (root, doc, other, private)
        };
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&doc, &other, &private])).await.unwrap();

        // Listed with the space's name, and each item can be restored or deleted, not only emptied as a whole
        let listed = list_trash(State(env.st.clone()), admin.clone(), Query(TrashQuery::default())).await.unwrap().0.into_items();
        let plan = listed.iter().find(|l| l.node.name == "plan.txt").unwrap();
        let space = plan.location_space.as_ref().unwrap();
        assert_eq!((space.kind, space.name.as_str(), plan.location_path.len()), (tree::SpaceKind::Team, "Team", 0));
        let listed: Vec<(String, String)> = listed.into_iter().map(|l| (l.node.name.clone(), l.location.clone())).collect();
        assert!(listed.contains(&("plan.txt".to_string(), "Team".to_string())), "{listed:?}");
        assert!(!listed.iter().any(|(n, _)| n == "diary.txt"), "personal spaces stay private: {listed:?}");
        let _ = restore(State(env.st.clone()), admin.clone(), ids(&[&doc])).await.unwrap();
        let _ = delete_forever(State(env.st.clone()), admin.clone(), ids(&[&other])).await.unwrap();
        assert_eq!(env.drive_of(&doc).await, env.drive_of(&team_root).await);
        // A personal space's trash is not the administrator's
        assert!(restore(State(env.st.clone()), admin.clone(), ids(&[&private])).await.is_err());
    }

    #[tokio::test]
    async fn folder_share_editor_cannot_take_folder_out_of_its_drive() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let shared = env.folder(&amy, amy.root(), "Shared with Ben").await;
        let inner = env.folder(&amy, &shared, "Inner").await;
        let doc = env.file(&amy, &shared, "report.txt").await;
        env.grant(&shared, &ben, "editor").await;
        let amy_drive = env.drive_of(&shared).await;

        // Ben is only an editor via a folder share: he can't move the folder or its files into his own space
        let err = move_nodes(State(env.st.clone()), ben.clone(), batch(&[&shared], ben.root())).await.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        let err = move_nodes(State(env.st.clone()), ben.clone(), batch(&[&doc], ben.root())).await.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(env.drive_of(&shared).await, amy_drive);
        assert_eq!(env.drive_of(&doc).await, amy_drive);

        // Organizing within the shared folder is fine
        let _ = move_nodes(State(env.st.clone()), ben.clone(), batch(&[&doc], &inner)).await.unwrap();
        assert_eq!(env.drive_of(&doc).await, amy_drive);

        // A space member (the owner) can move across spaces
        let company = env.st.shared_root().unwrap();
        let _ = move_nodes(State(env.st.clone()), amy.clone(), batch(&[&shared], &company)).await.unwrap();
        assert_eq!(env.drive_of(&shared).await, env.drive_of(&company).await);
        assert_eq!(env.drive_of(&doc).await, env.drive_of(&company).await);

        // Members of the company space (everyone can edit) can move its items into their own space
        let _ = move_nodes(State(env.st.clone()), ben.clone(), batch(&[&doc], ben.root())).await.unwrap();
        assert_eq!(env.drive_of(&doc).await, env.drive_of(ben.root()).await);
    }

    #[tokio::test]
    async fn space_usage_counter_follows_copies_moves_and_deletes() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, amy.root(), "docs").await;
        let doc = env.file(&amy, &folder, "a.bin").await;
        sqlx::query("UPDATE nodes SET size = 1000 WHERE id = ?").bind(&doc).execute(&env.st.db).await.unwrap();
        tree::recompute_usage(&env.st).await.unwrap();
        let db = env.st.db.clone();
        let used = |drive: String| {
            let db = db.clone();
            async move {
                let (u,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(drive).fetch_one(&db).await.unwrap();
                u
            }
        };
        let personal = env.drive_of(amy.root()).await;
        let company = env.drive_of(&env.st.shared_root().unwrap()).await;
        assert_eq!(used(personal.clone()).await, 1000);

        // Copy within the space: counted twice
        let _ = copy_nodes(State(env.st.clone()), amy.clone(), batch(&[&doc], amy.root())).await.unwrap();
        assert_eq!(used(personal.clone()).await, 2000);
        // Move the folder to the company space: bytes follow
        let _ = move_nodes(State(env.st.clone()), amy.clone(), batch(&[&folder], &env.st.shared_root().unwrap())).await.unwrap();
        assert_eq!(used(personal.clone()).await, 1000);
        assert_eq!(used(company.clone()).await, 1000);
        // Trash keeps counting (it still takes space); permanent deletion frees it
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&folder])).await.unwrap();
        assert_eq!(used(company.clone()).await, 1000);
        let _ = delete_forever(State(env.st.clone()), amy.clone(), ids(&[&folder])).await.unwrap();
        assert_eq!(used(company.clone()).await, 0);
        // The counters agree with the node table
        tree::recompute_usage(&env.st).await.unwrap();
        assert_eq!(used(personal).await, 1000);
    }

    #[tokio::test]
    async fn folder_contents_count_everything_inside_except_the_trash() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let top = env.folder(&amy, amy.root(), "top").await;
        let sub = env.folder(&amy, &top, "sub").await;
        let deep = env.folder(&amy, &sub, "deep").await;
        let gone = env.folder(&amy, &top, "gone").await;
        let other = env.folder(&amy, amy.root(), "other").await;
        for (parent, name, size) in [(&top, "a.txt", 100), (&sub, "b.txt", 20), (&deep, "c.txt", 3), (&gone, "d.txt", 4000), (&other, "e.txt", 5)] {
            let id = env.file(&amy, parent, name).await;
            sqlx::query("UPDATE nodes SET size = ? WHERE id = ?").bind(size).bind(&id).execute(&env.st.db).await.unwrap();
        }
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&gone])).await.unwrap();
        let of = |list: &[&str]| Json(ContentsReq { ids: list.iter().map(|s| s.to_string()).collect() });

        // The folder itself is not counted; the trashed folder and its file are left out
        let Json(c) = contents(State(env.st.clone()), amy.clone(), of(&[&top])).await.unwrap();
        assert_eq!(c, Contents { size: 123, files: 3, folders: 2 });
        // Several folders add up, a folder inside another selected one is counted once, and a file holds nothing
        let a = env.file(&amy, amy.root(), "loose.txt").await;
        let Json(c) = contents(State(env.st.clone()), amy.clone(), of(&[&top, &sub, &other, &a])).await.unwrap();
        assert_eq!(c, Contents { size: 128, files: 4, folders: 2 });
        let Json(c) = contents(State(env.st.clone()), amy.clone(), of(&[&deep])).await.unwrap();
        assert_eq!(c, Contents { size: 3, files: 1, folders: 0 });
        // Only for people who can see the folder
        let err = contents(State(env.st.clone()), ben.clone(), of(&[&top])).await.unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        env.grant(&sub, &ben, "viewer").await;
        let Json(c) = contents(State(env.st.clone()), ben.clone(), of(&[&sub])).await.unwrap();
        assert_eq!(c, Contents { size: 23, files: 2, folders: 1 });
    }

    #[tokio::test]
    async fn the_trash_shows_who_deleted_each_item_and_filters_by_me() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        // A team space both are members of
        let shared = {
            let mut conn = env.st.db.acquire().await.unwrap();
            let (_, root) = crate::db::create_drive(&mut conn, "Team", "team", amy.id, 0, "local").await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", amy.id, "owner", Some(amy.id), None).await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", ben.id, "editor", Some(amy.id), None).await.unwrap();
            root
        };
        let (mut by_amy, mut by_ben) = (Vec::new(), Vec::new());
        for i in 0..5 {
            by_amy.push(env.file(&amy, &shared, &format!("amy-{i}.txt")).await);
            by_ben.push(env.file(&amy, &shared, &format!("ben-{i}.txt")).await);
        }
        let old = env.file(&amy, &shared, "old.txt").await;
        for (who, list) in [(&amy, &by_amy), (&ben, &by_ben)] {
            let refs: Vec<&str> = list.iter().map(String::as_str).collect();
            let _ = trash(State(env.st.clone()), who.clone(), ids(&refs)).await.unwrap();
        }
        // Deleted before who deleted it was recorded
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&old])).await.unwrap();
        sqlx::query("UPDATE nodes SET trashed_by = NULL WHERE id = ?").bind(&old).execute(&env.st.db).await.unwrap();

        let all = list_trash(State(env.st.clone()), amy.clone(), Query(TrashQuery::default())).await.unwrap().0.into_items();
        assert_eq!(all.len(), 11);
        for l in &all {
            let expected = if l.node.name == "old.txt" { None } else { Some(&l.node.name[..3]) };
            assert_eq!(l.deleted_by.as_deref(), expected, "{}", l.node.name);
        }
        let unknown = all.iter().find(|l| l.node.name == "old.txt").unwrap();
        assert!(serde_json::to_value(unknown).unwrap().get("deleted_by").is_none());

        // Deleted by me, page by page
        for (who, prefix) in [(&amy, "amy-"), (&ben, "ben-")] {
            let (mut names, mut after) = (Vec::new(), None);
            loop {
                let q = TrashQuery { limit: Some(2), after, mine: Some(true) };
                let Json(Listing::Page { items, next, .. }) = list_trash(State(env.st.clone()), who.clone(), Query(q)).await.unwrap() else {
                    panic!("a limit gives a page")
                };
                names.extend(items.into_iter().map(|l| l.node.name));
                let Some(n) = next else { break };
                after = Some(n);
            }
            names.sort();
            assert_eq!(names, (0..5).map(|i| format!("{prefix}{i}.txt")).collect::<Vec<_>>());
        }

        // Restoring forgets it; deleting again records the new person
        let _ = restore(State(env.st.clone()), amy.clone(), ids(&[&by_ben[0]])).await.unwrap();
        let (by,): (Option<i64>,) = sqlx::query_as("SELECT trashed_by FROM nodes WHERE id = ?").bind(&by_ben[0]).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(by, None);
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&by_ben[0]])).await.unwrap();
        let mine = list_trash(State(env.st.clone()), amy.clone(), Query(TrashQuery { mine: Some(true), ..Default::default() })).await.unwrap().0.into_items();
        assert!(mine.iter().any(|l| l.node.id == by_ben[0]));
        assert_eq!(mine.len(), 6);
    }

    #[tokio::test]
    async fn recent_lists_files_i_opened_or_edited_in_shared_spaces_too() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let team = {
            let mut conn = env.st.db.acquire().await.unwrap();
            let (_, root) = crate::db::create_drive(&mut conn, "Team", "team", amy.id, 0, "local").await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", amy.id, "owner", Some(amy.id), None).await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", ben.id, "editor", Some(amy.id), None).await.unwrap();
            root
        };
        let t = now();
        let mine = env.file(&ben, ben.root(), "mine.txt").await;
        let opened = env.file(&amy, &team, "opened.txt").await;
        let edited = env.file(&amy, &team, "edited.txt").await;
        let _untouched = env.file(&amy, &team, "untouched.txt").await;
        sqlx::query("UPDATE nodes SET updated_at = ?").bind(t - 1000).execute(&env.st.db).await.unwrap();
        sqlx::query("UPDATE nodes SET updated_at = ? WHERE id = ?").bind(t - 300).bind(&mine).execute(&env.st.db).await.unwrap();
        record_open(&env.st, ben.id, &opened).await.unwrap();
        sqlx::query("UPDATE recent_files SET at = ?").bind(t - 100).execute(&env.st.db).await.unwrap();
        {
            let mut c = env.st.db.acquire().await.unwrap();
            let node = tree::get_node(&mut c, &edited).await.unwrap().unwrap();
            logs::record_activity(&mut c, &ben, Some(&node), "edit", "").await.unwrap();
        }
        sqlx::query("UPDATE activity SET at = ?").bind(t - 200).execute(&env.st.db).await.unwrap();
        let names = |who: User| {
            let st = env.st.clone();
            async move { recent(State(st), who).await.unwrap().0.into_iter().map(|l| l.node.name).collect::<Vec<_>>() }
        };

        // Newest first by the latest of: my own file's change, my open, my edit
        assert_eq!(names(ben.clone()).await, ["opened.txt", "edited.txt", "mine.txt"]);
        // Amy's own files are in hers; Ben opening them doesn't put them in hers
        assert!(!names(amy.clone()).await.contains(&"mine.txt".to_string()));

        // Opening again within a minute doesn't write; later it moves the file up
        let at = || async {
            let (at,): (i64,) = sqlx::query_as("SELECT at FROM recent_files WHERE user_id = ? AND node_id = ?").bind(ben.id).bind(&opened).fetch_one(&env.st.db).await.unwrap();
            at
        };
        sqlx::query("UPDATE recent_files SET at = ?").bind(t - 30).execute(&env.st.db).await.unwrap();
        record_open(&env.st, ben.id, &opened).await.unwrap();
        assert_eq!(at().await, t - 30);
        sqlx::query("UPDATE recent_files SET at = ?").bind(t - 120).execute(&env.st.db).await.unwrap();
        record_open(&env.st, ben.id, &opened).await.unwrap();
        assert!(at().await >= t);

        // Only files still reachable and not in the trash
        let _ = trash(State(env.st.clone()), ben.clone(), ids(&[&edited])).await.unwrap();
        assert_eq!(names(ben.clone()).await, ["opened.txt", "mine.txt"]);
        env.revoke(&team, &ben).await;
        assert_eq!(names(ben.clone()).await, ["mine.txt"]);

        // Only the latest few hundred opens are kept per person
        for i in 0..RECENT_OPENS_KEPT + 5 {
            let id = env.file(&ben, ben.root(), &format!("{i}.txt")).await;
            sqlx::query("INSERT INTO recent_files (user_id, node_id, at) VALUES (?, ?, ?)").bind(ben.id).bind(&id).bind(i).execute(&env.st.db).await.unwrap();
        }
        record_open(&env.st, ben.id, &mine).await.unwrap();
        let (kept, oldest): (i64, i64) =
            sqlx::query_as("SELECT COUNT(*), MIN(at) FROM recent_files WHERE user_id = ?").bind(ben.id).fetch_one(&env.st.db).await.unwrap();
        // With opened.txt and mine.txt, the 7 oldest go
        assert_eq!((kept, oldest), (RECENT_OPENS_KEPT, 7));
        // Deleting a file for good forgets it
        let _ = trash(State(env.st.clone()), ben.clone(), ids(&[&mine])).await.unwrap();
        let _ = delete_forever(State(env.st.clone()), ben.clone(), ids(&[&mine])).await.unwrap();
        let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM recent_files WHERE node_id = ?").bind(&mine).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(left, 0);
    }

    fn resolved(ids: &[&str], dest: Option<&str>, answer: Resolution) -> Json<BatchReq> {
        Json(BatchReq {
            ids: ids.iter().map(|s| s.to_string()).collect(),
            dest_id: dest.map(str::to_string),
            resolutions: ids.iter().map(|s| (s.to_string(), answer)).collect(),
        })
    }

    async fn name_of(env: &testutil::TestEnv, id: &str) -> (String, Option<String>, bool) {
        let n = tree::get_node(&mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap();
        (n.name, n.parent_id, n.trashed_at.is_some())
    }

    #[tokio::test]
    async fn moving_onto_a_taken_name_asks_and_then_replaces_skips_or_keeps_both() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let st = || State(env.st.clone());
        let dest = env.folder(&amy, amy.root(), "Dest").await;
        let there = env.file(&amy, &dest, "Report.docx").await;
        let src = env.folder(&amy, amy.root(), "Src").await;
        let a = env.file(&amy, &src, "report.docx").await;

        // The browser learns about the clash first
        let Json(found) = conflicts(st(), amy.clone(), Json(ConflictsReq { dest_id: Some(dest.clone()), names: vec!["REPORT.docx".into(), "new.txt".into()], ids: vec![a.clone()] }))
            .await
            .unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|c| c.existing.id == there));
        assert_eq!((found[0].id.as_deref(), found[1].id.as_deref()), (None, Some(a.as_str())));

        // Without an answer the move fails as before; skipped, nothing moves
        assert_eq!(move_nodes(st(), amy.clone(), batch(&[&a], &dest)).await.unwrap_err().status, StatusCode::CONFLICT);
        let _ = move_nodes(st(), amy.clone(), resolved(&[&a], Some(&dest), Resolution::Skip)).await.unwrap();
        assert_eq!(name_of(&env, &a).await.1.as_deref(), Some(src.as_str()));
        // Both kept: the moved one gets a number
        let _ = move_nodes(st(), amy.clone(), resolved(&[&a], Some(&dest), Resolution::Keep)).await.unwrap();
        assert_eq!(name_of(&env, &a).await, ("report (1).docx".into(), Some(dest.clone()), false));

        // Replaced: the item there goes to the trash, the moved one takes its place under its own name
        let b = env.file(&amy, &src, "Report.docx").await;
        let _ = move_nodes(st(), amy.clone(), resolved(&[&b], Some(&dest), Resolution::Replace)).await.unwrap();
        assert_eq!(name_of(&env, &b).await, ("Report.docx".into(), Some(dest.clone()), false));
        assert!(name_of(&env, &there).await.2, "the replaced file is in the trash");

        // A folder can't be replaced by something inside it
        let outer = env.folder(&amy, amy.root(), "Box").await;
        let inner = env.folder(&amy, &outer, "Box").await;
        let err = move_nodes(st(), amy.clone(), resolved(&[&inner], Some(amy.root()), Resolution::Replace)).await.unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert!(!name_of(&env, &outer).await.2);
    }

    #[tokio::test]
    async fn copying_and_restoring_onto_a_taken_name_follow_the_answer() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let st = || State(env.st.clone());
        let dest = env.folder(&amy, amy.root(), "Dest").await;
        let there = env.file(&amy, &dest, "a.txt").await;
        let a = env.file(&amy, amy.root(), "a.txt").await;
        let count = |parent: String| {
            let db = env.st.db.clone();
            async move {
                let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE parent_id = ? AND trashed_at IS NULL").bind(parent).fetch_one(&db).await.unwrap();
                n
            }
        };
        let _ = copy_nodes(st(), amy.clone(), resolved(&[&a], Some(&dest), Resolution::Skip)).await.unwrap();
        assert_eq!(count(dest.clone()).await, 1);
        // Without an answer a copy gets a number, as it always did
        let _ = copy_nodes(st(), amy.clone(), batch(&[&a], &dest)).await.unwrap();
        assert_eq!(count(dest.clone()).await, 2);
        let _ = copy_nodes(st(), amy.clone(), resolved(&[&a], Some(&dest), Resolution::Replace)).await.unwrap();
        assert_eq!(count(dest.clone()).await, 2);
        assert!(name_of(&env, &there).await.2);

        // Restoring: another "a.txt" took the name meanwhile
        let _ = trash(st(), amy.clone(), ids(&[&a])).await.unwrap();
        let newer = env.file(&amy, amy.root(), "a.txt").await;
        let Json(found) = conflicts(st(), amy.clone(), Json(ConflictsReq { dest_id: None, names: vec![], ids: vec![a.clone()] })).await.unwrap();
        assert_eq!(found.iter().map(|c| c.existing.id.as_str()).collect::<Vec<_>>(), [newer.as_str()]);
        let _ = restore(st(), amy.clone(), resolved(&[&a], None, Resolution::Skip)).await.unwrap();
        assert!(name_of(&env, &a).await.2, "skipped: still in the trash");
        let _ = restore(st(), amy.clone(), resolved(&[&a], None, Resolution::Replace)).await.unwrap();
        assert_eq!(name_of(&env, &a).await, ("a.txt".into(), Some(amy.root().to_string()), false));
        assert!(name_of(&env, &newer).await.2);
    }

    #[tokio::test]
    async fn replacing_in_a_folder_space_moves_the_item_there_to_its_trash() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        testutil::write_old(&space.dir.join("a.txt"), b"old");
        testutil::write_old(&space.dir.join("Sub/a.txt"), b"new");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (old, _) = env.node_at(&space.drive, "a.txt").await.unwrap();
        let (new, _) = env.node_at(&space.drive, "Sub/a.txt").await.unwrap();
        let _ = move_nodes(State(env.st.clone()), admin.clone(), resolved(&[&new], Some(&space.root), Resolution::Replace)).await.unwrap();
        assert_eq!(std::fs::read(space.dir.join("a.txt")).unwrap(), b"new");
        assert_eq!(env.node_at(&space.drive, "a.txt").await.unwrap().0, new);
        assert!(name_of(&env, &old).await.2);
        assert!(!space.dir.join("Sub/a.txt").exists());
    }

    #[tokio::test]
    async fn old_trash_is_purged_in_batches_with_its_content_references() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (shared, alone) = ("ab".repeat(32), "cd".repeat(32));
        // A file with the given content (5 bytes each)
        let file = async |parent: &str, name: String, hash: &str| {
            let id = env.file(&amy, parent, &name).await;
            let mut c = env.st.db.acquire().await.unwrap();
            tree::add_blob_ref(&mut c, hash, 5, "local").await.unwrap();
            sqlx::query("UPDATE nodes SET blob_hash = ?, size = 5 WHERE id = ?").bind(hash).bind(&id).execute(&mut *c).await.unwrap();
            id
        };
        // More old items than one batch takes: files, and a folder with files inside
        let mut old = Vec::new();
        for i in 0..130 {
            old.push(file(amy.root(), format!("old{i}.txt"), &shared).await);
        }
        let folder = env.folder(&amy, amy.root(), "Old folder").await;
        for i in 0..3 {
            file(&folder, format!("inner{i}.txt"), &alone).await;
        }
        old.push(folder);
        let recent = file(amy.root(), "recent.txt".into(), &shared).await;
        let live = file(amy.root(), "live.txt".into(), &shared).await;
        tree::recompute_usage(&env.st).await.unwrap();
        let refs: Vec<&str> = old.iter().map(String::as_str).collect();
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&refs)).await.unwrap();
        sqlx::query("UPDATE nodes SET trashed_at = trashed_at - 40 * 86400 WHERE trashed_at IS NOT NULL").execute(&env.st.db).await.unwrap();
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&recent])).await.unwrap();

        assert_eq!(purge_expired_trash(&env.st, 30).await.unwrap(), 131);
        let drive = env.drive_of(amy.root()).await;
        let (left,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = ? AND parent_id IS NOT NULL").bind(&drive).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(left, 2, "only the recent item in the trash and the live file are left");
        assert!(name_of(&env, &recent).await.2 && !name_of(&env, &live).await.2);
        // The content still used keeps its two references; the other one is no longer recorded
        let refcount = "SELECT refcount FROM blobs WHERE hash = ?";
        let shared_refs: Option<(i64,)> = sqlx::query_as(refcount).bind(&shared).fetch_optional(&env.st.db).await.unwrap();
        let alone_refs: Option<(i64,)> = sqlx::query_as(refcount).bind(&alone).fetch_optional(&env.st.db).await.unwrap();
        assert_eq!((shared_refs, alone_refs), (Some((2,)), None));
        let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(&drive).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(used, 10);
        tree::recompute_usage(&env.st).await.unwrap();
        let (again,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(&drive).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(again, used, "the counter agrees with the files");
        // Nothing more is old enough
        assert_eq!(purge_expired_trash(&env.st, 30).await.unwrap(), 0);
    }
}
