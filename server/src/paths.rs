//! Paths that name items the way the user sees them: `/My files/Reports/2026.xlsx`. The first segment is one of the
//! user's spaces ("My files" for their own personal space, other spaces by name) or "Shared with me" (items shared from
//! spaces the user isn't a member of); below it is the folder tree. Names that clash get a number: "Projects (2)".
//! WebDAV serves this tree at `/dav/`, and the address bar on the web shows and accepts the same paths.

use std::collections::{HashMap, HashSet};

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    state::AppState,
    tree::{self, NODE_COLS, Node},
    util::numbered_name,
};

/// The folder of items shared from spaces the user isn't a member of
pub const SHARED: &str = "Shared with me";
/// The user's own personal space
pub const MY_FILES: &str = "My files";
/// Longest path the address bar may ask for
const MAX_PATH: usize = 4096;

pub fn same_name(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

/// The top level: each space and each item shared with the user, under a name unique among them
pub struct Tops {
    /// (name, root folder id)
    pub spaces: Vec<(String, String)>,
    /// (name, item)
    pub shared: Vec<(String, Node)>,
}

/// Adds the first of `base`, `numbered(2)`, `numbered(3)`… that isn't taken yet (ignoring letter case)
fn claim(taken: &mut HashSet<String>, base: &str, numbered: impl Fn(u32) -> String) -> String {
    let mut name = base.to_string();
    let mut n = 2;
    while !taken.insert(name.to_lowercase()) {
        name = numbered(n);
        n += 1;
    }
    name
}

pub async fn tops(conn: &mut SqliteConnection, user: &User) -> AppResult<Tops> {
    let mut drives = tree::user_drives(conn, user).await?;
    let rank = |k: &str| match k {
        "personal" => 0,
        "company" => 1,
        _ => 2,
    };
    // A fixed order, so that numbered names stay with the same space
    drives.sort_by(|a, b| rank(&a.0.kind).cmp(&rank(&b.0.kind)).then_with(|| a.0.name.cmp(&b.0.name)).then_with(|| a.0.id.cmp(&b.0.id)));
    let member_of: Vec<String> = drives.iter().map(|(d, _)| d.id.clone()).collect();
    let mut taken = HashSet::from([SHARED.to_lowercase()]);
    let spaces = drives
        .into_iter()
        .map(|(d, _)| {
            let base = if d.kind == "personal" && d.owner_id == Some(user.id) { MY_FILES.to_string() } else { d.name.clone() };
            (claim(&mut taken, &base, |n| format!("{base} ({n})")), d.root_id)
        })
        .collect();
    let mut items = tree::shared_with_me_outside(conn, user, &member_of).await?;
    items.sort_by(|a, b| a.0.name.cmp(&b.0.name).then_with(|| a.0.id.cmp(&b.0.id)));
    let mut taken = HashSet::new();
    let shared = items
        .into_iter()
        .map(|(n, _, _)| {
            let name = claim(&mut taken, &n.name, |k| numbered_name(&n.name, k, n.is_folder()));
            (name, n)
        })
        .collect();
    Ok(Tops { spaces, shared })
}

pub enum Target {
    /// `/`: all of the user's spaces
    Root,
    /// `/Shared with me/`
    Shared,
    Node(Box<Node>),
}

pub struct Found {
    pub target: Target,
    /// The path as it is named here (the names of the items, not as the request spelled them)
    pub path: Vec<String>,
}

/// The item in a folder with this name (not in the trash): any letter case in the content store, exact in folder spaces
pub async fn child_named(conn: &mut SqliteConnection, parent_id: &str, name: &str) -> AppResult<Option<Node>> {
    let sql = format!(
        "SELECT {NODE_COLS} FROM nodes n WHERE n.parent_id = ?1 AND n.trashed_at IS NULL
         AND n.name_key = CASE WHEN n.fs_path IS NULL THEN unicode_lower(?2) ELSE ?2 END LIMIT 1"
    );
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(parent_id).bind(name).fetch_optional(conn).await?)
}

/// Finds what a path names; None when it doesn't exist or the user has no access to it
pub async fn resolve(conn: &mut SqliteConnection, user: &User, segs: &[String]) -> AppResult<Option<Found>> {
    let tops = tops(conn, user).await?;
    resolve_in(conn, user, &tops, segs).await
}

async fn resolve_in(conn: &mut SqliteConnection, user: &User, tops: &Tops, segs: &[String]) -> AppResult<Option<Found>> {
    let Some((first, rest)) = segs.split_first() else { return Ok(Some(Found { target: Target::Root, path: Vec::new() })) };
    let (mut path, start, rest) = if same_name(first, SHARED) {
        let Some((item, rest)) = rest.split_first() else { return Ok(Some(Found { target: Target::Shared, path: vec![SHARED.into()] })) };
        let Some((name, node)) = tops.shared.iter().find(|(name, _)| same_name(name, item)) else { return Ok(None) };
        (vec![SHARED.to_string(), name.clone()], node.id.clone(), rest)
    } else {
        let Some((name, root)) = tops.spaces.iter().find(|(name, _)| same_name(name, first)) else { return Ok(None) };
        (vec![name.clone()], root.clone(), rest)
    };
    let mut current = start;
    for (i, seg) in rest.iter().enumerate() {
        let Some(child) = child_named(conn, &current, seg).await? else { return Ok(None) };
        if i + 1 < rest.len() && !child.is_folder() {
            return Ok(None);
        }
        path.push(child.name.clone());
        current = child.id;
    }
    match tree::node_with_role(conn, user, &current).await {
        Ok((node, _)) => Ok(Some(Found { target: Target::Node(Box::new(node)), path })),
        Err(e) if e.status == StatusCode::NOT_FOUND => Ok(None),
        Err(e) => Err(e),
    }
}

/// The path that names this item for the user (what `resolve` finds it by); None when no path reaches it, as for an
/// administrator looking after a space they aren't a member of
pub async fn location_of(conn: &mut SqliteConnection, user: &User, node: &Node) -> AppResult<Option<Vec<String>>> {
    let tops = tops(conn, user).await?;
    let path = tree::path_of(conn, &node.id).await?;
    let root = match &node.parent_id {
        None => node.id.clone(),
        Some(_) => match tree::get_drive(conn, node.drive()).await? {
            Some(d) => d.root_id,
            None => return Ok(None),
        },
    };
    if let Some((name, _)) = tops.spaces.iter().find(|(_, r)| *r == root) {
        return Ok(Some(std::iter::once(name.clone()).chain(path.into_iter().map(|c| c.name)).collect()));
    }
    // Through a folder share: from the shared item down
    for (i, crumb) in path.iter().enumerate() {
        if let Some((name, _)) = tops.shared.iter().find(|(_, n)| n.id == crumb.id) {
            let below = path[i + 1..].iter().map(|c| c.name.clone());
            return Ok(Some([SHARED.to_string(), name.clone()].into_iter().chain(below).collect()));
        }
    }
    Ok(None)
}

/// The segments of a typed or pasted path: `/` and `\` both separate, blanks around names are dropped, `.` stays
/// and `..` goes up. A WebDAV address (`https://…/dav/My%20files/…` or `/dav/…`) is read as the path it names.
fn split_path(raw: &str) -> Option<Vec<String>> {
    let mut raw = raw.trim();
    if let Some((_, rest)) = raw.split_once("://") {
        raw = rest.find('/').map_or("", |i| &rest[i..]);
    }
    let decoded;
    if let Some(rest) = raw.strip_prefix("/dav").filter(|r| r.is_empty() || r.starts_with('/')) {
        decoded = percent_encoding::percent_decode_str(rest).decode_utf8().ok()?.into_owned();
        raw = &decoded;
    }
    let mut segs: Vec<String> = Vec::new();
    for seg in raw.split(['/', '\\']).map(str::trim).filter(|s| !s.is_empty()) {
        match seg {
            "." => {}
            ".." => {
                segs.pop();
            }
            _ => segs.push(seg.to_string()),
        }
    }
    Some(segs)
}

#[derive(Deserialize)]
pub struct FindReq {
    path: String,
    /// Names of the top level as the browser shows them in its language (e.g. "我的檔案") → the name used here
    /// ("My files"); used when the first segment isn't a name at the top level itself
    #[serde(default)]
    aliases: HashMap<String, String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct FindRes {
    /// `spaces` (all spaces), `shared` (Shared with me), `folder` or `file`
    place: &'static str,
    /// The folder or file
    id: Option<String>,
    /// The path as it is named (letter case as stored, numbered names as shown)
    path: Vec<String>,
}

/// What a path typed or pasted into the address bar names
pub async fn find(State(st): State<AppState>, user: User, Json(req): Json<FindReq>) -> AppResult<Json<FindRes>> {
    let missing = || AppError::not_found("Nothing was found at this path");
    if req.path.len() > MAX_PATH {
        return Err(missing());
    }
    let mut segs = split_path(&req.path).ok_or_else(missing)?;
    let mut c = st.db.acquire().await?;
    let tops = tops(&mut c, &user).await?;
    if let Some(first) = segs.first_mut()
        && !same_name(first, SHARED)
        && !tops.spaces.iter().any(|(name, _)| same_name(name, first))
        && let Some((_, name)) = req.aliases.iter().find(|(alias, _)| same_name(alias, first))
    {
        *first = name.clone();
    }
    let found = resolve_in(&mut c, &user, &tops, &segs).await?.ok_or_else(missing)?;
    let (place, id) = match found.target {
        Target::Root => ("spaces", None),
        Target::Shared => ("shared", None),
        Target::Node(n) => (if n.is_folder() { "folder" } else { "file" }, Some(n.id)),
    };
    Ok(Json(FindRes { place, id, path: found.path }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, TestEnv};
    use serde_json::json;

    async fn find_path(env: &TestEnv, user: &User, path: &str, aliases: serde_json::Value) -> AppResult<FindRes> {
        let req = serde_json::from_value(json!({ "path": path, "aliases": aliases })).unwrap();
        find(State(env.st.clone()), user.clone(), Json(req)).await.map(|Json(r)| r)
    }

    #[test]
    fn typed_paths_are_split() {
        let s = |p: &str| split_path(p).unwrap();
        assert_eq!(s("/My files/Reports/"), ["My files", "Reports"]);
        assert_eq!(s("My files\\Reports\\ 2026.xlsx "), ["My files", "Reports", "2026.xlsx"]);
        assert_eq!(s("/My files/./a/../b"), ["My files", "b"]);
        assert_eq!(s("/../.."), Vec::<String>::new());
        assert_eq!(s("  /  "), Vec::<String>::new());
        assert_eq!(s("https://files.example.com/dav/My%20files/a%2Bb/"), ["My files", "a+b"]);
        assert_eq!(s("/dav/Shared%20with%20me"), ["Shared with me"]);
        // Only a path that starts with /dav is a WebDAV address
        assert_eq!(s("/david/x"), ["david", "x"]);
        assert!(split_path("/dav/%FF").is_none());
    }

    #[tokio::test]
    async fn paths_find_spaces_folders_files_and_shared_items() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let amy = env.user("amy", true).await;
        let bob = env.user("bob", true).await;
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        let notes = env.file(&amy, &docs, "notes.txt").await;
        // A team space whose name clashes with "My files", and a folder Bob shares with Amy
        let req = serde_json::from_value(json!({ "name": "My files" })).unwrap();
        let Json(team) = crate::drives::create(State(env.st.clone()), admin, Json(req)).await.unwrap();
        let team = serde_json::to_value(&team).unwrap();
        let team_root = team["root_id"].as_str().unwrap().to_string();
        env.grant(&team_root, &amy, "editor").await;
        let plans = env.folder(&bob, bob.root(), "Plans").await;
        let q1 = env.folder(&bob, &plans, "Q1").await;
        env.grant(&plans, &amy, "viewer").await;
        let none = json!({});

        let r = find_path(&env, &amy, "/", none.clone()).await.unwrap();
        assert_eq!((r.place, r.id, r.path.len()), ("spaces", None, 0));
        let r = find_path(&env, &amy, "shared with ME", none.clone()).await.unwrap();
        assert_eq!(r.place, "shared");
        let r = find_path(&env, &amy, "/my files/DOCS/Notes.TXT", none.clone()).await.unwrap();
        assert_eq!(r, FindRes { place: "file", id: Some(notes.clone()), path: vec!["My files".into(), "Docs".into(), "notes.txt".into()] });
        let r = find_path(&env, &amy, "/My files (2)", none.clone()).await.unwrap();
        assert_eq!((r.place, r.id.as_deref()), ("folder", Some(team_root.as_str())));
        let r = find_path(&env, &amy, "/Shared with me/Plans/Q1", none.clone()).await.unwrap();
        assert_eq!((r.place, r.id.as_deref()), ("folder", Some(q1.as_str())));

        // Names as the browser shows them in its language
        let aliases = json!({ "我的檔案": "My files", "與我共用": "Shared with me" });
        let r = find_path(&env, &amy, "/我的檔案/Docs", aliases.clone()).await.unwrap();
        assert_eq!(r.id.as_deref(), Some(docs.as_str()));
        let r = find_path(&env, &amy, "/與我共用/Plans", aliases).await.unwrap();
        assert_eq!(r.path, ["Shared with me", "Plans"]);

        // What doesn't exist, isn't reachable or goes through a file isn't found
        for path in ["/Nowhere", "/My files/Missing", "/My files/Docs/notes.txt/x", "/Shared with me/Secret", "/My files (3)"] {
            let e = find_path(&env, &amy, path, none.clone()).await.unwrap_err();
            assert_eq!(e.status, StatusCode::NOT_FOUND, "{path}");
        }
        // Bob's folder is under his own "My files", not Amy's
        let r = find_path(&env, &bob, "/My files/Plans", none.clone()).await.unwrap();
        assert_eq!(r.id.as_deref(), Some(plans.as_str()));
        assert!(find_path(&env, &bob, "/My files/Docs", none).await.is_err());

        // The path of an item is the one it is found by
        let mut c = env.st.db.acquire().await.unwrap();
        let loc = |id: String| {
            let (amy, st) = (amy.clone(), env.st.clone());
            async move {
                let mut c = st.db.acquire().await.unwrap();
                let (node, _) = tree::node_with_role(&mut c, &amy, &id).await.unwrap();
                location_of(&mut c, &amy, &node).await.unwrap()
            }
        };
        assert_eq!(loc(notes).await.unwrap(), ["My files", "Docs", "notes.txt"]);
        assert_eq!(loc(amy.root().to_string()).await.unwrap(), ["My files"]);
        assert_eq!(loc(team_root).await.unwrap(), ["My files (2)"]);
        assert_eq!(loc(q1).await.unwrap(), ["Shared with me", "Plans", "Q1"]);
        assert!(tree::node_with_role(&mut c, &amy, bob.root()).await.is_err());
    }
}
