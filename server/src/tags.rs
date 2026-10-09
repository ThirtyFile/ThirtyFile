//! Coloured tags: labels each person puts on files and folders, with a name and a colour from a fixed palette.
//!
//! Tags are private. A person's tags, their names and what they are on are only ever sent to that person: every read
//! here is by the signed-in person's id, listings mark only their own tags on items (`tree::mark_own`), and nothing
//! about tags goes into the activity log, notifications, share links or the Control panel. So someone who can see an
//! item in a shared space, an administrator, or a visitor of a share link never learns who tagged it, or how.
//!
//! A person can tag any item they can see, also in read-only spaces: tagging changes nothing about the item. Once they
//! can no longer see an item, its tags don't show anywhere (the listings only hold items they can open), and they come
//! back with the access.
//!
//! Assignments are on the item's node (`tagged`, migrations/0020_tags.sql), so they follow it through moves, into the
//! trash and back. Copies get the copier's tags of the original (`copy_tags`).

use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    state::AppState,
    tree::{self, Need},
    util::now,
};

/// The palette, as stored and as the API names it. The browser draws each in its own shade for each style and theme.
pub const COLORS: [&str; 7] = ["red", "orange", "yellow", "green", "blue", "purple", "gray"];
/// Tags one person may have
const MAX_TAGS: i64 = 200;
/// Characters in a tag's name
const MAX_NAME: usize = 64;
/// Items tagged or untagged in one request (a large selection comes a batch at a time, web/src/lib/span.ts)
const MAX_ITEMS: usize = 1000;
/// Tags added or removed in one request
const MAX_CHANGED: usize = 50;

#[derive(Debug, Serialize, sqlx::FromRow, PartialEq)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub color: String,
}

/// The signed-in person's tags, by name
pub async fn list(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<Tag>>> {
    let tags = sqlx::query_as("SELECT id, name, color FROM tags WHERE owner_id = ? ORDER BY name COLLATE natural_name, id").bind(user.id).fetch_all(&st.db).await?;
    Ok(Json(tags))
}

#[derive(Deserialize)]
pub struct TagReq {
    name: Option<String>,
    color: Option<String>,
}

fn valid_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("Enter a name for the tag"));
    }
    if name.chars().count() > MAX_NAME {
        return Err(AppError::bad_request(format!("A tag's name can be at most {MAX_NAME} characters")));
    }
    if name.chars().any(char::is_control) {
        return Err(AppError::bad_request("A tag's name can't contain control characters"));
    }
    Ok(name.to_string())
}

fn valid_color(color: &str) -> AppResult<&'static str> {
    COLORS.into_iter().find(|c| *c == color).ok_or_else(|| AppError::bad_request("Choose one of the tag colors"))
}

/// Refuses a name another of the person's tags has (letter case doesn't count)
async fn name_free(conn: &mut SqliteConnection, user: &User, name: &str, except: i64) -> AppResult<()> {
    let taken: Option<(i64,)> = sqlx::query_as("SELECT id FROM tags WHERE owner_id = ? AND name_key = unicode_lower(?) AND id != ?")
        .bind(user.id)
        .bind(name)
        .bind(except)
        .fetch_optional(&mut *conn)
        .await?;
    match taken {
        Some(_) => Err(AppError::conflict(format!("You already have a tag named \"{name}\""))),
        None => Ok(()),
    }
}

/// The person's own tag `id`; any other is "not found", whoever's it is
async fn own(conn: &mut SqliteConnection, user: &User, id: i64) -> AppResult<Tag> {
    sqlx::query_as("SELECT id, name, color FROM tags WHERE id = ? AND owner_id = ?")
        .bind(id)
        .bind(user.id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::not_found("Tag not found"))
}

/// Refuses unless every one of `ids` is the person's own tag (a filter by someone else's tag would tell what they tagged)
pub async fn check_own(conn: &mut SqliteConnection, user: &User, ids: &[i64]) -> AppResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let (found,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tags WHERE owner_id = ? AND id IN (SELECT value FROM json_each(?))")
        .bind(user.id)
        .bind(serde_json::to_string(ids).unwrap())
        .fetch_one(&mut *conn)
        .await?;
    let distinct = ids.iter().collect::<std::collections::HashSet<_>>().len();
    if found as usize != distinct {
        return Err(AppError::not_found("Tag not found"));
    }
    Ok(())
}

pub async fn create(State(st): State<AppState>, user: User, Json(req): Json<TagReq>) -> AppResult<Json<Tag>> {
    let name = valid_name(req.name.as_deref().unwrap_or_default())?;
    let color = valid_color(req.color.as_deref().unwrap_or_default())?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tags WHERE owner_id = ?").bind(user.id).fetch_one(&mut *tx).await?;
    if count >= MAX_TAGS {
        return Err(AppError::bad_request(format!("You can have at most {MAX_TAGS} tags")));
    }
    name_free(&mut tx, &user, &name, 0).await?;
    let (id,): (i64,) = sqlx::query_as("INSERT INTO tags (owner_id, name, color, created_at) VALUES (?, ?, ?, ?) RETURNING id")
        .bind(user.id)
        .bind(&name)
        .bind(color)
        .bind(now())
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(Tag { id, name, color: color.to_string() }))
}

/// Renames or recolours one of the person's tags
pub async fn update(State(st): State<AppState>, user: User, Path(id): Path<i64>, Json(req): Json<TagReq>) -> AppResult<Json<Tag>> {
    let name = req.name.as_deref().map(valid_name).transpose()?;
    let color = req.color.as_deref().map(valid_color).transpose()?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let mut tag = own(&mut tx, &user, id).await?;
    if let Some(name) = name {
        name_free(&mut tx, &user, &name, id).await?;
        tag.name = name;
    }
    if let Some(color) = color {
        tag.color = color.to_string();
    }
    sqlx::query("UPDATE tags SET name = ?, color = ? WHERE id = ?").bind(&tag.name).bind(&tag.color).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(tag))
}

/// Deletes one of the person's tags, and with it every assignment of it
pub async fn delete(State(st): State<AppState>, user: User, Path(id): Path<i64>) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    own(&mut tx, &user, id).await?;
    sqlx::query("DELETE FROM tags WHERE id = ?").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ApplyReq {
    ids: Vec<String>,
    #[serde(default)]
    add: Vec<i64>,
    #[serde(default)]
    remove: Vec<i64>,
}

/// Adds tags to items and removes tags from them, for up to 1,000 items at once. A tag both added and removed is
/// removed.
pub async fn apply(State(st): State<AppState>, user: User, Json(req): Json<ApplyReq>) -> AppResult<Json<Value>> {
    if req.ids.is_empty() || req.ids.len() > MAX_ITEMS {
        return Err(AppError::bad_request("Select 1 to 1000 items"));
    }
    if req.add.len() + req.remove.len() > MAX_CHANGED {
        return Err(AppError::bad_request(format!("Change at most {MAX_CHANGED} tags at once")));
    }
    let add: Vec<i64> = req.add.iter().copied().filter(|t| !req.remove.contains(t)).collect();
    // Checked before taking the write lock, which every other change waits for: a check is a few queries per item
    let mut items = Vec::new();
    {
        let mut c = st.db.acquire().await?;
        for id in &req.ids {
            // Any item the person can see, and nothing else: someone else's item is "not found"
            let node = tree::node_for(&mut c, &user, id, Need::Read).await?;
            if node.parent_id.is_none() {
                return Err(AppError::bad_request("This can't be done on the root folder of a space"));
            }
            items.push(node.id);
        }
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    check_own(&mut tx, &user, &req.add).await?;
    check_own(&mut tx, &user, &req.remove).await?;
    let (items, at) = (serde_json::to_string(&items).unwrap(), now());
    // Items that went to the trash meanwhile get no new tags
    sqlx::query(
        "INSERT OR IGNORE INTO tagged (node_id, tag_id, owner_id, created_at)
         SELECT i.value, t.value, ?3, ?4 FROM json_each(?1) i JOIN nodes n ON n.id = i.value AND n.trashed_at IS NULL, json_each(?2) t",
    )
    .bind(&items)
    .bind(serde_json::to_string(&add).unwrap())
    .bind(user.id)
    .bind(at)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM tagged WHERE owner_id = ?3 AND node_id IN (SELECT value FROM json_each(?1)) AND tag_id IN (SELECT value FROM json_each(?2))")
        .bind(&items)
        .bind(serde_json::to_string(&req.remove).unwrap())
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

/// Gives copies the tags their originals have, of the person who copies: `pairs` maps each original's id to its copy's
pub async fn copy_tags<'a>(conn: &mut SqliteConnection, user_id: i64, pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> AppResult<()> {
    let pairs: Vec<[&str; 2]> = pairs.into_iter().map(|(from, to)| [from, to]).collect();
    if pairs.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT OR IGNORE INTO tagged (node_id, tag_id, owner_id, created_at)
         SELECT json_extract(p.value, '$[1]'), t.tag_id, t.owner_id, ?3
         FROM json_each(?1) p JOIN tagged t ON t.node_id = json_extract(p.value, '$[0]') AND t.owner_id = ?2",
    )
    .bind(serde_json::to_string(&pairs).unwrap())
    .bind(user_id)
    .bind(now())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;

    use axum::{
        extract::{Path, Query, State},
        http::{HeaderMap, StatusCode},
    };
    use serde_json::json;

    use super::*;
    use crate::testutil::{self, TestEnv, write_old};

    fn req<T: serde::de::DeserializeOwned>(v: Value) -> Json<T> {
        Json(serde_json::from_value(v).unwrap())
    }

    fn query<T: serde::de::DeserializeOwned>(v: Value) -> Query<T> {
        Query(serde_json::from_value(v).unwrap())
    }

    fn value(v: impl Serialize) -> Value {
        serde_json::to_value(v).unwrap()
    }

    /// A new tag of `user`'s (red)
    pub async fn make(env: &TestEnv, user: &User, name: &str) -> i64 {
        create(State(env.st.clone()), user.clone(), req(json!({ "name": name, "color": "red" }))).await.unwrap().0.id
    }

    /// Adds and removes `user`'s tags on items
    pub async fn tag(env: &TestEnv, user: &User, ids: &[&str], add: &[i64], remove: &[i64]) -> AppResult<()> {
        apply(State(env.st.clone()), user.clone(), req(json!({ "ids": ids, "add": add, "remove": remove }))).await.map(|_| ())
    }

    async fn names(env: &TestEnv, user: &User) -> Vec<(String, String)> {
        list(State(env.st.clone()), user.clone()).await.unwrap().0.into_iter().map(|t| (t.name, t.color)).collect()
    }

    fn pair(name: &str, color: &str) -> (String, String) {
        (name.to_string(), color.to_string())
    }

    /// The tags `user` sees on an item, opened on its own
    async fn tags_on(env: &TestEnv, user: &User, id: &str) -> Vec<i64> {
        let Json(info) = crate::nodes::get(State(env.st.clone()), user.clone(), Path(id.to_string())).await.unwrap();
        serde_json::from_value(value(info)["node"].get("tags").cloned().unwrap_or(json!([]))).unwrap()
    }

    /// The items of a folder as `user` lists them (with `q`), by name, with the tags they see on each
    async fn listed(env: &TestEnv, user: &User, folder: &str, q: Value) -> HashMap<String, Vec<i64>> {
        let Json(list) = crate::nodes::children(State(env.st.clone()), user.clone(), Path(folder.to_string()), query(q)).await.unwrap();
        list.into_items().into_iter().map(|n| (n.name, n.tags)).collect()
    }

    fn sorted_names(found: &Value) -> Vec<String> {
        let mut names: Vec<String> = found["items"].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap().to_string()).collect();
        names.sort();
        names
    }

    /// Names of the items `user` finds with their tag
    async fn with_tag(env: &TestEnv, user: &User, tag: i64) -> Vec<String> {
        let Json(found) = crate::nodes::tagged(State(env.st.clone()), user.clone(), Path(tag), query(json!({}))).await.unwrap();
        sorted_names(&value(found))
    }

    async fn search(env: &TestEnv, user: &User, q: Value) -> AppResult<Vec<String>> {
        let Json(found) = crate::nodes::search(State(env.st.clone()), user.clone(), query(q)).await?;
        Ok(sorted_names(&value(found)))
    }

    async fn tagged_rows(env: &TestEnv, id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tagged WHERE node_id = ?").bind(id).fetch_one(&env.st.db).await.unwrap()
    }

    /// The item named `name` in `folder` (not in the trash)
    async fn child(env: &TestEnv, folder: &str, name: &str) -> String {
        sqlx::query_scalar("SELECT id FROM nodes WHERE parent_id = ? AND name = ? AND trashed_at IS NULL")
            .bind(folder)
            .bind(name)
            .fetch_one(&env.st.db)
            .await
            .unwrap()
    }

    /// Waits for a job working in the background to have done what `done` checks
    async fn eventually(mut done: impl AsyncFnMut() -> bool) -> bool {
        for _ in 0..250 {
            if done().await {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        false
    }

    #[tokio::test]
    async fn each_person_has_their_own_tags_each_name_once() {
        let env = testutil::env().await;
        let (amy, ben) = (env.user("amy", true).await, env.user("ben", true).await);
        let st = || State(env.st.clone());
        let urgent = make(&env, &amy, "Urgent").await;
        let Json(later) = create(st(), amy.clone(), req(json!({ "name": "  Later ", "color": "blue" }))).await.unwrap();
        assert_eq!(later.name, "Later");
        assert_eq!(names(&env, &amy).await, [pair("Later", "blue"), pair("Urgent", "red")]);

        // Refused: a name taken (letter case doesn't count), no name, a long name, control characters, a colour off the
        // palette
        let status = |r: AppResult<Json<Tag>>| r.unwrap_err().status;
        assert_eq!(status(create(st(), amy.clone(), req(json!({ "name": "URGENT", "color": "red" }))).await), StatusCode::CONFLICT);
        assert_eq!(status(create(st(), amy.clone(), req(json!({ "name": " ", "color": "red" }))).await), StatusCode::BAD_REQUEST);
        assert_eq!(status(create(st(), amy.clone(), req(json!({ "name": "x".repeat(65), "color": "red" }))).await), StatusCode::BAD_REQUEST);
        assert_eq!(status(create(st(), amy.clone(), req(json!({ "name": "a\nb", "color": "red" }))).await), StatusCode::BAD_REQUEST);
        assert_eq!(status(create(st(), amy.clone(), req(json!({ "name": "Pink", "color": "pink" }))).await), StatusCode::BAD_REQUEST);
        // Another person can have the same name
        make(&env, &ben, "Urgent").await;

        // Renamed and recoloured; a tag can change the letter case of its own name
        let Json(t) = update(st(), amy.clone(), Path(urgent), req(json!({ "name": "Today" }))).await.unwrap();
        assert_eq!((t.name.as_str(), t.color.as_str()), ("Today", "red"));
        let Json(t) = update(st(), amy.clone(), Path(later.id), req(json!({ "color": "green" }))).await.unwrap();
        assert_eq!((t.name.as_str(), t.color.as_str()), ("Later", "green"));
        assert_eq!(status(update(st(), amy.clone(), Path(later.id), req(json!({ "name": "today" }))).await), StatusCode::CONFLICT);
        let _ = update(st(), amy.clone(), Path(urgent), req(json!({ "name": "TODAY" }))).await.unwrap();
        assert_eq!(names(&env, &amy).await, [pair("Later", "green"), pair("TODAY", "red")]);

        // Someone else's tag is not found, whatever they do with it
        assert_eq!(status(update(st(), ben.clone(), Path(urgent), req(json!({ "color": "blue" }))).await), StatusCode::NOT_FOUND);
        assert_eq!(delete(st(), ben.clone(), Path(urgent)).await.unwrap_err().status, StatusCode::NOT_FOUND);
        assert_eq!(names(&env, &ben).await, [pair("Urgent", "red")]);

        // Deleted with its assignments
        let file = env.file(&amy, amy.root(), "a.txt").await;
        tag(&env, &amy, &[&file], &[urgent, later.id], &[]).await.unwrap();
        let _ = delete(st(), amy.clone(), Path(urgent)).await.unwrap();
        assert_eq!(tags_on(&env, &amy, &file).await, [later.id]);
        assert_eq!(names(&env, &amy).await, [pair("Later", "green")]);

        // At most MAX_TAGS each
        sqlx::query(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?)
             INSERT INTO tags (owner_id, name, color, created_at) SELECT ?, 'Tag ' || i, 'gray', 0 FROM n",
        )
        .bind(MAX_TAGS - 1)
        .bind(amy.id)
        .execute(&env.st.db)
        .await
        .unwrap();
        assert_eq!(status(create(st(), amy.clone(), req(json!({ "name": "One more", "color": "red" }))).await), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn nobody_else_sees_a_persons_tags() {
        let env = testutil::env().await;
        let (amy, ben) = (env.user("amy", true).await, env.user("ben", true).await);
        let admin = env.admin().await;
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        let a = env.stored_file(&amy, &docs, "a.txt", b"a").await;
        env.grant(&docs, &ben, "editor").await;
        let (urgent, mine) = (make(&env, &amy, "Urgent").await, make(&env, &ben, "Mine").await);
        tag(&env, &amy, &[&docs, &a], &[urgent], &[]).await.unwrap();
        tag(&env, &ben, &[&a], &[mine], &[]).await.unwrap();

        // Each sees their own on the same item
        assert_eq!(listed(&env, &amy, &docs, json!({})).await["a.txt"], [urgent]);
        assert_eq!(listed(&env, &ben, &docs, json!({})).await["a.txt"], [mine]);
        assert_eq!(tags_on(&env, &ben, &a).await, [mine]);
        assert!(tags_on(&env, &ben, &docs).await.is_empty());
        assert_eq!(with_tag(&env, &amy, urgent).await, ["Docs", "a.txt"]);

        // Someone else's tag can't be used to look for, or put on, anything
        let not_found = |r: AppResult<()>| assert_eq!(r.unwrap_err().status, StatusCode::NOT_FOUND);
        not_found(search(&env, &ben, json!({ "q": "", "tags": urgent.to_string() })).await.map(|_| ()));
        not_found(crate::nodes::tagged(State(env.st.clone()), ben.clone(), Path(urgent), query(json!({}))).await.map(|_| ()));
        not_found(crate::nodes::children(State(env.st.clone()), ben.clone(), Path(docs.clone()), query(json!({ "tag": urgent }))).await.map(|_| ()));
        not_found(tag(&env, &ben, &[&a], &[urgent], &[]).await);
        not_found(tag(&env, &ben, &[&a], &[], &[urgent]).await);
        assert_eq!(tags_on(&env, &amy, &a).await, [urgent]);
        // Nor can an item they can't see be tagged
        let private = env.file(&amy, amy.root(), "private.txt").await;
        not_found(tag(&env, &ben, &[&private], &[mine], &[]).await);

        // An administrator sees no one's tags, also in the space everyone shares
        let company = amy.shared_root.clone().unwrap();
        let plan = env.file(&amy, &company, "plan.txt").await;
        tag(&env, &amy, &[&plan], &[urgent], &[]).await.unwrap();
        assert!(listed(&env, &admin, &company, json!({})).await["plan.txt"].is_empty());
        assert!(tags_on(&env, &admin, &plan).await.is_empty());
        assert!(names(&env, &admin).await.is_empty());
        let Json(found) = crate::nodes::search(State(env.st.clone()), admin.clone(), query(json!({ "q": "plan" }))).await.unwrap();
        assert!(value(found)["items"][0].get("tags").is_none());

        // Nor does a visitor of a share link, even asking for a tag
        let link = serde_json::from_value(json!({ "node_id": docs })).unwrap();
        let Json(link) = crate::shares::create(State(env.st.clone()), amy.clone(), Json(link)).await.unwrap();
        let token = value(&link)["id"].as_str().unwrap().to_string();
        let visit = |q: Value| crate::shares::public_children(State(env.st.clone()), Path((token.clone(), docs.clone())), query(q), HeaderMap::new());
        let items = visit(json!({})).await.unwrap().0.into_items();
        assert_eq!(items.len(), 1);
        assert!(items[0].get("tags").is_none(), "{items:?}");
        assert!(visit(json!({ "tag": urgent })).await.unwrap().0.into_items().is_empty());
        let Json(node) = crate::shares::public_node(State(env.st.clone()), Path((token.clone(), a.clone())), HeaderMap::new()).await.unwrap();
        assert!(value(node)["node"].get("tags").is_none());

        // Tagging isn't recorded where others look
        let (logged,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM activity WHERE detail LIKE '%Urgent%' OR detail LIKE '%Mine%'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(logged, 0);

        // Without access, the tag shows nowhere; with it again, it is back
        env.revoke(&docs, &ben).await;
        assert!(with_tag(&env, &ben, mine).await.is_empty());
        assert!(search(&env, &ben, json!({ "q": "", "tags": mine.to_string() })).await.unwrap().is_empty());
        env.grant(&docs, &ben, "viewer").await;
        assert_eq!(with_tag(&env, &ben, mine).await, ["a.txt"]);
    }

    #[tokio::test]
    async fn tags_go_with_items_through_moves_copies_and_the_trash() {
        let env = testutil::env().await;
        let (amy, ben) = (env.user("amy", true).await, env.user("ben", true).await);
        let st = || State(env.st.clone());
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        let sub = env.folder(&amy, &docs, "Sub").await;
        let a = env.stored_file(&amy, &docs, "a.txt", b"a").await;
        let b = env.stored_file(&amy, &sub, "b.txt", b"b").await;
        let other = env.folder(&amy, amy.root(), "Other").await;
        env.grant(&docs, &ben, "editor").await;
        let (urgent, mine) = (make(&env, &amy, "Urgent").await, make(&env, &ben, "Mine").await);
        tag(&env, &amy, &[&docs, &a, &b], &[urgent], &[]).await.unwrap();
        tag(&env, &ben, &[&a], &[mine], &[]).await.unwrap();

        // Moved within the space, and to another space: the same items, with their tags
        let _ = crate::nodes::move_nodes(st(), amy.clone(), req(json!({ "ids": [a], "dest_id": sub }))).await.unwrap();
        assert_eq!(tags_on(&env, &amy, &a).await, [urgent]);
        let company = amy.shared_root.clone().unwrap();
        let _ = crate::nodes::move_nodes(st(), amy.clone(), req(json!({ "ids": [docs], "dest_id": company }))).await.unwrap();
        assert_eq!(env.drive_of(&b).await, env.drive_of(&company).await);
        assert_eq!((tags_on(&env, &amy, &docs).await, tags_on(&env, &amy, &b).await), (vec![urgent], vec![urgent]));
        assert_eq!(tags_on(&env, &ben, &a).await, [mine]);

        // Copied: the copies get the tags of whoever copies, nobody else's
        let _ = crate::nodes::copy_nodes(st(), amy.clone(), req(json!({ "ids": [docs], "dest_id": other }))).await.unwrap();
        let copy = child(&env, &other, "Docs").await;
        let copy_sub = child(&env, &copy, "Sub").await;
        let (copy_a, copy_b) = (child(&env, &copy_sub, "a.txt").await, child(&env, &copy_sub, "b.txt").await);
        assert_ne!(copy, docs);
        assert_eq!(tags_on(&env, &amy, &copy).await, [urgent]);
        assert!(tags_on(&env, &amy, &copy_sub).await.is_empty());
        assert_eq!((tags_on(&env, &amy, &copy_a).await, tags_on(&env, &amy, &copy_b).await), (vec![urgent], vec![urgent]));
        assert_eq!(tagged_rows(&env, &copy_a).await, 1, "not ben's");
        let _ = crate::nodes::copy_nodes(st(), ben.clone(), req(json!({ "ids": [a], "dest_id": ben.root() }))).await.unwrap();
        let bens = child(&env, ben.root(), "a.txt").await;
        assert_eq!(tags_on(&env, &ben, &bens).await, [mine]);
        assert_eq!(tagged_rows(&env, &bens).await, 1, "not amy's");

        // In the trash they stay, but the items aren't listed with their tag; restored, they are again
        let _ = crate::nodes::trash(st(), amy.clone(), req(json!({ "ids": [docs] }))).await.unwrap();
        assert_eq!(with_tag(&env, &amy, urgent).await, ["Docs", "a.txt", "b.txt"], "only the copies");
        assert_eq!(tagged_rows(&env, &a).await, 2);
        let _ = crate::nodes::restore(st(), amy.clone(), req(json!({ "ids": [docs] }))).await.unwrap();
        assert_eq!(with_tag(&env, &amy, urgent).await, ["Docs", "Docs", "a.txt", "a.txt", "b.txt", "b.txt"]);

        // Deleted for good: the assignments go with the items
        let _ = crate::nodes::trash(st(), amy.clone(), req(json!({ "ids": [docs] }))).await.unwrap();
        let _ = crate::nodes::delete_forever(st(), amy.clone(), req(json!({ "ids": [docs] }))).await.unwrap();
        let gone = async || tagged_rows(&env, &a).await + tagged_rows(&env, &b).await + tagged_rows(&env, &docs).await == 0;
        assert!(eventually(gone).await);
        assert_eq!(with_tag(&env, &amy, urgent).await, ["Docs", "a.txt", "b.txt"]);

        // A deleted account's tags go with it
        let admin = env.admin().await;
        let removal = query(json!({ "delete_files": true }));
        let _ = crate::admin::delete(State(env.st.clone()), crate::auth::Admin(admin), Path(ben.id), removal).await.unwrap();
        let bens_tags = async || sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tags WHERE owner_id = ?").bind(ben.id).fetch_one(&env.st.db).await.unwrap() == 0;
        assert!(eventually(bens_tags).await);
        assert_eq!(tagged_rows(&env, &copy_a).await, 1, "amy's stay");
    }

    #[tokio::test]
    async fn tags_stay_on_items_of_folder_spaces_changed_outside_thirtyfile() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let st = || State(env.st.clone());
        write_old(&space.dir.join("Docs/a.txt"), b"alpha");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (docs, _) = env.node_at(&space.drive, "Docs").await.unwrap();
        let (a, _) = env.node_at(&space.drive, "Docs/a.txt").await.unwrap();
        let urgent = make(&env, &admin, "Urgent").await;
        tag(&env, &admin, &[&docs, &a], &[urgent], &[]).await.unwrap();

        // Renamed in ThirtyFile
        let _ = crate::nodes::rename(st(), admin.clone(), Path(a.clone()), req(json!({ "name": "b.txt" }))).await.unwrap();
        assert_eq!(tags_on(&env, &admin, &a).await, [urgent]);

        // Copied within the folder space, and into the content store: the copies have the tag
        let _ = crate::nodes::copy_nodes(st(), admin.clone(), req(json!({ "ids": [a], "dest_id": space.root }))).await.unwrap();
        let (on_disk, _) = env.node_at(&space.drive, "b.txt").await.unwrap();
        let _ = crate::nodes::copy_nodes(st(), admin.clone(), req(json!({ "ids": [a], "dest_id": admin.root() }))).await.unwrap();
        let stored = child(&env, admin.root(), "b.txt").await;
        assert_eq!((tags_on(&env, &admin, &on_disk).await, tags_on(&env, &admin, &stored).await), (vec![urgent], vec![urgent]));

        // Moved from the content store into the folder space: the same item, with its tag
        let Json(dest) = crate::nodes::create_folder(st(), admin.clone(), req(json!({ "parent_id": space.root, "name": "Moved" }))).await.unwrap();
        let _ = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [stored], "dest_id": dest.id }))).await.unwrap();
        assert_eq!(env.node_at(&space.drive, "Moved/b.txt").await.map(|n| n.0), Some(stored.clone()));
        assert_eq!(tags_on(&env, &admin, &stored).await, [urgent]);

        // Renamed and moved on the server: the scan recognises the same items by their identity on disk (where the disk
        // gives one) and they keep their tags
        std::fs::rename(space.dir.join("Docs"), space.dir.join("Papers")).unwrap();
        std::fs::create_dir_all(space.dir.join("Inbox")).unwrap();
        std::fs::rename(space.dir.join("Papers/b.txt"), space.dir.join("Inbox/b.txt")).unwrap();
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (papers, _) = env.node_at(&space.drive, "Papers").await.unwrap();
        let (moved, _) = env.node_at(&space.drive, "Inbox/b.txt").await.unwrap();
        if cfg!(unix) {
            assert_eq!((papers.as_str(), moved.as_str()), (docs.as_str(), a.as_str()));
            assert_eq!((tags_on(&env, &admin, &papers).await, tags_on(&env, &admin, &moved).await), (vec![urgent], vec![urgent]));
            assert_eq!(with_tag(&env, &admin, urgent).await, ["Papers", "b.txt", "b.txt", "b.txt"]);
        }
    }

    #[tokio::test]
    async fn items_are_found_by_their_tags() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        let (a, b) = (env.file(&amy, &docs, "a.txt").await, env.file(&amy, &docs, "b.txt").await);
        let c = env.file(&amy, amy.root(), "c.txt").await;
        env.file(&amy, &docs, "report.txt").await;
        let (urgent, later) = (make(&env, &amy, "Urgent").await, make(&env, &amy, "Later").await);
        tag(&env, &amy, &[&a, &b], &[urgent], &[]).await.unwrap();
        tag(&env, &amy, &[&b, &c], &[later], &[]).await.unwrap();

        // Search: by tag alone, by several (all of them), with a name, within a folder
        assert_eq!(search(&env, &amy, json!({ "q": "", "tags": urgent.to_string() })).await.unwrap(), ["a.txt", "b.txt"]);
        assert_eq!(search(&env, &amy, json!({ "q": "", "tags": format!("{urgent},{later}") })).await.unwrap(), ["b.txt"]);
        assert_eq!(search(&env, &amy, json!({ "q": "a", "tags": urgent.to_string() })).await.unwrap(), ["a.txt"]);
        assert_eq!(search(&env, &amy, json!({ "q": "", "tags": later.to_string(), "in": docs })).await.unwrap(), ["b.txt"]);
        assert!(search(&env, &amy, json!({ "q": "" })).await.unwrap().is_empty(), "nothing to look for");
        assert_eq!(search(&env, &amy, json!({ "q": "", "tags": "x" })).await.unwrap_err().status, StatusCode::NOT_FOUND);

        // A folder, whole or a page at a time
        let listed = listed(&env, &amy, &docs, json!({ "tag": urgent })).await;
        assert_eq!(listed.len(), 2);
        assert_eq!(listed["b.txt"], [urgent, later]);
        let q = query(json!({ "tag": later, "limit": 10, "offset": 0 }));
        let Json(page) = crate::nodes::children(State(env.st.clone()), amy.clone(), Path(docs.clone()), q).await.unwrap();
        assert_eq!(value(page)["total"], 1);

        // Everything with a tag; added and removed at once, it is removed
        assert_eq!(with_tag(&env, &amy, later).await, ["b.txt", "c.txt"]);
        tag(&env, &amy, &[&c], &[later], &[later]).await.unwrap();
        assert_eq!(with_tag(&env, &amy, later).await, ["b.txt"]);
    }

    #[tokio::test]
    async fn a_large_selection_is_tagged_a_batch_at_a_time() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let big = env.folder(&amy, amy.root(), "Big").await;
        sqlx::query(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2500)
             INSERT INTO nodes (id, owner_id, parent_id, kind, name, size, mime, drive_id, created_at, updated_at)
             SELECT 'big' || i, ?1, ?2, 'file', 'File ' || i, 0, 'text/plain', (SELECT drive_id FROM nodes WHERE id = ?2), 0, 0 FROM n",
        )
        .bind(amy.id)
        .bind(&big)
        .execute(&env.st.db)
        .await
        .unwrap();
        let urgent = make(&env, &amy, "Urgent").await;

        // Everything but two: the browser gets the ids a batch at a time and tags each batch (web/src/lib/span.ts)
        let mut after: Option<String> = None;
        let mut batches = 0;
        loop {
            let ask = json!({ "except": ["big1", "big2"], "after": after });
            let Json(page) = crate::nodes::select(State(env.st.clone()), amy.clone(), Path(big.clone()), req(ask)).await.unwrap();
            let page = value(page);
            let ids: Vec<&str> = page["ids"].as_array().unwrap().iter().map(|i| i.as_str().unwrap()).collect();
            tag(&env, &amy, &ids, &[urgent], &[]).await.unwrap();
            batches += 1;
            match page["next"].as_str() {
                Some(next) => after = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(batches, 3);
        let Json(found) = crate::nodes::tagged(State(env.st.clone()), amy.clone(), Path(urgent), query(json!({}))).await.unwrap();
        let found = value(found);
        assert_eq!((found["items"].as_array().unwrap().len(), &found["truncated"]), (2498, &json!(false)));
        let listed = listed(&env, &amy, &big, json!({ "limit": 5 })).await;
        assert!(listed["File 1"].is_empty());
        assert_eq!(listed["File 3"], [urgent]);

        // More than one batch at once, none, or a space's top folder: refused
        let many: Vec<String> = (1..=1001).map(|i| format!("big{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(tag(&env, &amy, &many, &[urgent], &[]).await.unwrap_err().status, StatusCode::BAD_REQUEST);
        assert_eq!(tag(&env, &amy, &[], &[urgent], &[]).await.unwrap_err().status, StatusCode::BAD_REQUEST);
        assert_eq!(tag(&env, &amy, &[amy.root()], &[urgent], &[]).await.unwrap_err().status, StatusCode::BAD_REQUEST);
    }
}
