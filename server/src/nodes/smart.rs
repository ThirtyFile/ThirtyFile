//! Smart folders: searches a person saved, which show as folders and stay current ("PDFs changed this month",
//! "everything tagged Urgent").
//!
//! A smart folder holds no items, only what it looks for (migrations/0021_smart_folders.sql): deleting one deletes
//! nothing else, and moving an item never puts it in one. Smart folders are private: only their owner lists, opens or
//! changes them, and anyone else's is "not found", also for administrators.
//!
//! What a smart folder lists is worked out each time it is listed, with the search's criteria (find.rs) against the
//! index, never by walking disks, and only among the items the person can open at that moment: access is checked when
//! listing, not when the query was saved. It lists like a folder (list.rs): a page at a time in any order, the
//! position of an item and selecting a range, so a large one works as a large folder does.

use super::*;

/// Smart folders one person may have
const MAX_SMART: i64 = 100;
/// Characters in a smart folder's name
const MAX_NAME: usize = 100;
/// Characters of the name to look for
const MAX_TERM: usize = 255;
/// File types (extensions) one smart folder looks for
const MAX_EXTS: usize = 50;
/// Tags one smart folder looks for
const MAX_TAGS: usize = 20;
/// The longest period "modified in the last days" can be
const MAX_DAYS: i64 = 36_500;

/// Where a smart folder looks
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Scope {
    /// Every space and shared folder the person can open
    #[default]
    All,
    /// One space
    Space { id: String },
    /// A folder and its subfolders
    Folder { id: String },
}

/// What a smart folder looks for: the parts left out don't narrow it down, and an item matches all the others
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct SmartQuery {
    /// In the name
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// "file" or "folder"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Files with one of these extensions: comma separated, lowercase, without the dot
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<String>,
    /// Files of this size in bytes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<i64>,
    /// Modified at or after / before (Unix seconds)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_from: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_to: Option<i64>,
    /// Modified in the last this many days, counted from when it is listed (instead of dates)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_days: Option<i64>,
    #[serde(default)]
    pub scope: Scope,
    /// With all of these tags of the owner's
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<i64>,
}

impl SmartQuery {
    /// The query as it is kept: tidied, and refused when it looks for nothing or for something it can't. Where it
    /// looks must be somewhere the person can open now; a folder is kept by its id (the alias "root" too).
    async fn checked(mut self, conn: &mut SqliteConnection, user: &User) -> AppResult<Self> {
        self.name = self.name.trim().to_string();
        if self.name.chars().count() > MAX_TERM {
            return Err(AppError::bad_request(format!("The name to look for can be at most {MAX_TERM} characters")));
        }
        if self.name.chars().any(char::is_control) {
            return Err(AppError::bad_request("The name to look for can't contain control characters"));
        }
        if !matches!(self.kind.as_deref(), None | Some("file") | Some("folder")) {
            return Err(AppError::bad_request("Look for files, folders or both"));
        }
        let exts = ext_list(self.ext.as_deref());
        if exts.len() > MAX_EXTS {
            return Err(AppError::bad_request(format!("Choose at most {MAX_EXTS} file types")));
        }
        if exts.iter().any(|e| e.chars().count() > 16 || !e.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')) {
            return Err(AppError::bad_request("Enter file types as extensions, such as pdf or docx"));
        }
        self.ext = (!exts.is_empty()).then(|| exts.join(","));
        if self.min_size.is_some_and(|s| s < 0) || self.max_size.is_some_and(|s| s < 0) {
            return Err(AppError::bad_request("Sizes can't be less than 0"));
        }
        if let (Some(min), Some(max)) = (self.min_size, self.max_size)
            && min > max
        {
            return Err(AppError::bad_request("The smallest size is larger than the largest"));
        }
        if let (Some(from), Some(to)) = (self.modified_from, self.modified_to)
            && from >= to
        {
            return Err(AppError::bad_request("The first date is after the last"));
        }
        if let Some(days) = self.modified_days {
            if !(1..=MAX_DAYS).contains(&days) {
                return Err(AppError::bad_request(format!("Choose a period of 1 to {MAX_DAYS} days")));
            }
            if self.modified_from.is_some() || self.modified_to.is_some() {
                return Err(AppError::bad_request("Choose either a period or dates, not both"));
            }
        }
        self.tags.sort_unstable();
        self.tags.dedup();
        if self.tags.len() > MAX_TAGS {
            return Err(AppError::bad_request(format!("Choose at most {MAX_TAGS} tags")));
        }
        crate::tags::check_own(conn, user, &self.tags).await?;
        let looks_for = !self.name.is_empty()
            || self.kind.is_some()
            || self.ext.is_some()
            || self.min_size.is_some()
            || self.max_size.is_some()
            || self.modified_from.is_some()
            || self.modified_to.is_some()
            || self.modified_days.is_some()
            || !self.tags.is_empty();
        if !looks_for {
            return Err(AppError::bad_request("Choose something for the smart folder to look for"));
        }
        match &mut self.scope {
            Scope::All => {}
            Scope::Space { id } => {
                if !tree::member_of(conn, user).await?.contains(id) {
                    return Err(AppError::not_found("Space not found"));
                }
            }
            Scope::Folder { id } => match tree::folder_for(conn, user, id, Need::Read).await {
                Ok(folder) => *id = folder.id,
                Err(e) if e.status == axum::http::StatusCode::BAD_REQUEST => return Err(AppError::bad_request("Choose a folder to look in")),
                Err(e) => return Err(e),
            },
        }
        Ok(self)
    }

    /// What the query looks for now, as `user` may see it; None when where it looks can no longer be opened (it then
    /// lists nothing)
    async fn criteria(&self, conn: &mut SqliteConnection, user: &User) -> AppResult<Option<Criteria>> {
        let (mut below, mut space) = (None, None);
        match &self.scope {
            Scope::All => {}
            // Of a space, only what the person can open in it (Criteria::held)
            Scope::Space { id } => space = Some(id.clone()),
            Scope::Folder { id } => match tree::folder_for(conn, user, id, Need::Read).await {
                Ok(folder) => below = Some(folder.id),
                Err(e) if e.status.is_client_error() => return Ok(None),
                Err(e) => return Err(e),
            },
        }
        // A period ends now; to the minute, so the pages of one listing agree
        let (from, to) = match self.modified_days {
            Some(days) => (Some(now() / 60 * 60 - days * 86_400), None),
            None => (self.modified_from, self.modified_to),
        };
        Ok(Some(Criteria {
            term: self.name.clone(),
            below,
            space,
            kind: self.kind.clone(),
            exts: ext_list(self.ext.as_deref()),
            from,
            to,
            min_size: self.min_size,
            max_size: self.max_size,
            owner: None,
            tags: self.tags.clone(),
            tagged_by: user.id,
        }))
    }
}

#[derive(Serialize, Debug)]
pub struct SmartFolder {
    id: i64,
    name: String,
    query: SmartQuery,
}

#[derive(sqlx::FromRow)]
struct SmartRow {
    id: i64,
    name: String,
    query: String,
}

impl TryFrom<SmartRow> for SmartFolder {
    type Error = AppError;
    fn try_from(r: SmartRow) -> AppResult<Self> {
        Ok(SmartFolder { id: r.id, name: r.name, query: serde_json::from_str(&r.query).map_err(AppError::internal)? })
    }
}

/// The person's own smart folder `id`; any other is "not found", whoever's it is
async fn own(conn: &mut SqliteConnection, user: &User, id: i64) -> AppResult<SmartFolder> {
    let row: SmartRow = sqlx::query_as("SELECT id, name, query FROM smart_folders WHERE id = ? AND owner_id = ?")
        .bind(id)
        .bind(user.id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::not_found("Smart folder not found"))?;
    row.try_into()
}

/// The signed-in person's smart folders, by name
pub async fn smart_folders(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<SmartFolder>>> {
    let rows: Vec<SmartRow> = sqlx::query_as("SELECT id, name, query FROM smart_folders WHERE owner_id = ? ORDER BY name COLLATE natural_name, id")
        .bind(user.id)
        .fetch_all(&st.db)
        .await?;
    Ok(Json(rows.into_iter().map(SmartFolder::try_from).collect::<AppResult<_>>()?))
}

pub async fn smart_folder(State(st): State<AppState>, user: User, Path(id): Path<i64>) -> AppResult<Json<SmartFolder>> {
    Ok(Json(own(&mut *st.db.acquire().await?, &user, id).await?))
}

#[derive(Deserialize)]
pub struct SmartReq {
    name: Option<String>,
    query: Option<SmartQuery>,
}

fn valid_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("Enter a name for the smart folder"));
    }
    if name.chars().count() > MAX_NAME {
        return Err(AppError::bad_request(format!("A smart folder's name can be at most {MAX_NAME} characters")));
    }
    if name.chars().any(char::is_control) {
        return Err(AppError::bad_request("A smart folder's name can't contain control characters"));
    }
    Ok(name.to_string())
}

/// Refuses a name another of the person's smart folders has (letter case doesn't count)
async fn name_free(conn: &mut SqliteConnection, user: &User, name: &str, except: i64) -> AppResult<()> {
    let taken: Option<(i64,)> = sqlx::query_as("SELECT id FROM smart_folders WHERE owner_id = ? AND name_key = unicode_lower(?) AND id != ?")
        .bind(user.id)
        .bind(name)
        .bind(except)
        .fetch_optional(&mut *conn)
        .await?;
    match taken {
        Some(_) => Err(AppError::conflict(format!("You already have a smart folder named \"{name}\""))),
        None => Ok(()),
    }
}

/// Saves a search as a smart folder
pub async fn create_smart_folder(State(st): State<AppState>, user: User, Json(req): Json<SmartReq>) -> AppResult<Json<SmartFolder>> {
    let name = valid_name(req.name.as_deref().unwrap_or_default())?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let query = req.query.unwrap_or_default().checked(&mut tx, &user).await?;
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM smart_folders WHERE owner_id = ?").bind(user.id).fetch_one(&mut *tx).await?;
    if count >= MAX_SMART {
        return Err(AppError::bad_request(format!("You can have at most {MAX_SMART} smart folders")));
    }
    name_free(&mut tx, &user, &name, 0).await?;
    let at = now();
    let (id,): (i64,) = sqlx::query_as("INSERT INTO smart_folders (owner_id, name, query, created_at, updated_at) VALUES (?, ?, ?, ?, ?) RETURNING id")
        .bind(user.id)
        .bind(&name)
        .bind(serde_json::to_string(&query).unwrap())
        .bind(at)
        .bind(at)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(SmartFolder { id, name, query }))
}

/// Renames a smart folder, or changes what it looks for
pub async fn update_smart_folder(State(st): State<AppState>, user: User, Path(id): Path<i64>, Json(req): Json<SmartReq>) -> AppResult<Json<SmartFolder>> {
    let name = req.name.as_deref().map(valid_name).transpose()?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let mut folder = own(&mut tx, &user, id).await?;
    if let Some(name) = name {
        name_free(&mut tx, &user, &name, id).await?;
        folder.name = name;
    }
    if let Some(query) = req.query {
        folder.query = query.checked(&mut tx, &user).await?;
    }
    sqlx::query("UPDATE smart_folders SET name = ?, query = ?, updated_at = ? WHERE id = ?")
        .bind(&folder.name)
        .bind(serde_json::to_string(&folder.query).unwrap())
        .bind(now())
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(folder))
}

/// Deletes a smart folder: only the saved search, never the items it lists
pub async fn delete_smart_folder(State(st): State<AppState>, user: User, Path(id): Path<i64>) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    own(&mut tx, &user, id).await?;
    sqlx::query("DELETE FROM smart_folders WHERE id = ?").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

/// What the person's smart folder `id` holds for them now
async fn held_by(conn: &mut SqliteConnection, user: &User, id: i64) -> AppResult<Held> {
    let folder = own(conn, user, id).await?;
    let Some(criteria) = folder.query.criteria(conn, user).await? else { return Ok(Held::new("0".into(), Vec::new())) };
    let (drives, folders) = tree::scope(conn, user).await?;
    Ok(criteria.held(drives, folders))
}

/// The items a smart folder lists, with where each is, as a folder's are listed: a page at a time (at most MAX_PAGE),
/// by its `next` or from a position (with how many there are)
pub async fn smart_items(State(st): State<AppState>, user: User, Path(id): Path<i64>, Query(q): Query<ListQuery>) -> AppResult<Json<Listing<Located>>> {
    let q = ListQuery { folders_only: None, tag: None, tagged_by: None, ..q }.paged();
    let mut c = st.db.acquire().await?;
    let held = held_by(&mut c, &user, id).await?;
    let list = list_held(&mut c, &held, "", &q).await?;
    drop(c);
    Ok(Json(match list {
        Listing::All(items) => Listing::All(locate(&st, &user, items).await?),
        Listing::Page { items, next, total } => Listing::Page { items: locate(&st, &user, items).await?, next, total },
    }))
}

/// Where an item is in a smart folder's listing
pub async fn smart_position(State(st): State<AppState>, user: User, Path(id): Path<i64>, Query(q): Query<PositionQuery>) -> AppResult<Json<Position>> {
    let mut c = st.db.acquire().await?;
    let held = held_by(&mut c, &user, id).await?;
    Ok(Json(position_in(&mut c, &held, &q).await?))
}

/// Items selected in a smart folder not all loaded in the browser, a batch at a time (as `select` in a folder)
pub async fn smart_select(State(st): State<AppState>, user: User, Path(id): Path<i64>, Json(req): Json<SelectReq>) -> AppResult<Json<Selected>> {
    let mut c = st.db.acquire().await?;
    let held = held_by(&mut c, &user, id).await?;
    Ok(Json(select_in(&mut c, &held, &req).await?))
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, StatusCode};
    use serde_json::json;

    use super::*;
    use crate::{
        tags::tests::{make as make_tag, tag},
        testutil::{self, TestEnv},
    };

    fn req<T: serde::de::DeserializeOwned>(v: Value) -> Json<T> {
        Json(serde_json::from_value(v).unwrap())
    }

    fn query<T: serde::de::DeserializeOwned>(v: Value) -> Query<T> {
        Query(serde_json::from_value(v).unwrap())
    }

    /// A new smart folder of `user`'s
    async fn make(env: &TestEnv, user: &User, name: &str, q: Value) -> AppResult<i64> {
        Ok(create_smart_folder(State(env.st.clone()), user.clone(), req(json!({ "name": name, "query": q }))).await?.0.id)
    }

    /// One page of a smart folder as `user` lists it with `q`: the names, `next` and `total`
    async fn page(env: &TestEnv, user: &User, id: i64, q: Value) -> AppResult<(Vec<String>, Option<String>, Option<i64>)> {
        let Json(list) = smart_items(State(env.st.clone()), user.clone(), Path(id), query(q)).await?;
        let Listing::Page { items, next, total } = list else { panic!("a smart folder lists a page at a time") };
        Ok((items.into_iter().map(|l| l.node.name).collect(), next, total))
    }

    /// What a smart folder lists for `user`, by name
    async fn names(env: &TestEnv, user: &User, id: i64) -> Vec<String> {
        page(env, user, id, json!({})).await.unwrap().0
    }

    /// What a smart folder looking for `q` lists for `user`, by name
    async fn found(env: &TestEnv, user: &User, q: Value) -> Vec<String> {
        let id = make(env, user, &format!("Search {}", new_id()), q).await.unwrap();
        names(env, user, id).await
    }

    async fn set(env: &TestEnv, id: &str, size: i64, updated_at: i64) {
        sqlx::query("UPDATE nodes SET size = ?, updated_at = ? WHERE id = ?").bind(size).bind(updated_at).bind(id).execute(&env.st.db).await.unwrap();
    }

    async fn count_nodes(env: &TestEnv) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM nodes WHERE trashed_at IS NULL").fetch_one(&env.st.db).await.unwrap()
    }

    #[tokio::test]
    async fn each_person_has_their_own_smart_folders_which_hold_no_items() {
        let env = testutil::env().await;
        let (amy, ben, admin) = (env.user("amy", true).await, env.user("ben", true).await, env.admin().await);
        let st = || State(env.st.clone());
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        let a = env.file(&amy, &docs, "a.pdf").await;
        let pdfs = make(&env, &amy, "  PDFs ", json!({ "ext": ".PDF, docx" })).await.unwrap();
        let Json(saved) = smart_folder(st(), amy.clone(), Path(pdfs)).await.unwrap();
        assert_eq!((saved.name.as_str(), saved.query.ext.as_deref(), &saved.query.scope), ("PDFs", Some("pdf,docx"), &Scope::All));
        assert_eq!(names(&env, &amy, pdfs).await, ["a.pdf"]);

        // Refused: a name taken (letter case doesn't count), no name, a long one, nothing to look for, and what can't be
        // looked for
        let status = |r: AppResult<i64>| r.unwrap_err().status;
        let bad = StatusCode::BAD_REQUEST;
        assert_eq!(status(make(&env, &amy, "pdfs", json!({ "name": "x" })).await), StatusCode::CONFLICT);
        assert_eq!(status(make(&env, &amy, " ", json!({ "name": "x" })).await), bad);
        assert_eq!(status(make(&env, &amy, &"x".repeat(MAX_NAME + 1), json!({ "name": "x" })).await), bad);
        assert_eq!(status(make(&env, &amy, "Nothing", json!({})).await), bad);
        assert_eq!(status(make(&env, &amy, "Nothing", json!({ "name": "  ", "scope": { "kind": "folder", "id": docs } })).await), bad);
        for q in [
            json!({ "kind": "link" }),
            json!({ "ext": "p df" }),
            json!({ "min_size": -1 }),
            json!({ "min_size": 10, "max_size": 9 }),
            json!({ "modified_from": 10, "modified_to": 10 }),
            json!({ "modified_days": 0 }),
            json!({ "modified_days": 7, "modified_from": 10 }),
            json!({ "name": "a\u{7}" }),
            json!({ "name": "x", "scope": { "kind": "folder", "id": a } }),
        ] {
            assert_eq!(status(make(&env, &amy, "Bad", q.clone()).await), bad, "{q}");
        }
        // Someone else's tag, folder or space is not found
        let bens_tag = make_tag(&env, &ben, "Mine").await;
        let bens_folder = env.folder(&ben, ben.root(), "Plans").await;
        let bens_space = env.drive_of(ben.root()).await;
        for q in [
            json!({ "tags": [bens_tag] }),
            json!({ "name": "x", "scope": { "kind": "folder", "id": bens_folder } }),
            json!({ "name": "x", "scope": { "kind": "space", "id": bens_space } }),
        ] {
            assert_eq!(status(make(&env, &amy, "Theirs", q.clone()).await), StatusCode::NOT_FOUND, "{q}");
        }
        // A folder is kept by its id
        let home = make(&env, &amy, "Home", json!({ "name": "a", "scope": { "kind": "folder", "id": "root" } })).await.unwrap();
        assert_eq!(smart_folder(st(), amy.clone(), Path(home)).await.unwrap().0.query.scope, Scope::Folder { id: amy.root().to_string() });

        // Nobody else sees, opens, lists or changes them, an administrator neither
        for other in [&ben, &admin] {
            assert!(smart_folders(st(), other.clone()).await.unwrap().0.is_empty());
            let not_found = |s: StatusCode| assert_eq!(s, StatusCode::NOT_FOUND);
            not_found(smart_folder(st(), other.clone(), Path(pdfs)).await.unwrap_err().status);
            not_found(page(&env, other, pdfs, json!({})).await.unwrap_err().status);
            not_found(smart_position(st(), other.clone(), Path(pdfs), query(json!({ "item": a }))).await.unwrap_err().status);
            not_found(smart_select(st(), other.clone(), Path(pdfs), req(json!({}))).await.unwrap_err().status);
            not_found(update_smart_folder(st(), other.clone(), Path(pdfs), req(json!({ "name": "Mine" }))).await.unwrap_err().status);
            not_found(delete_smart_folder(st(), other.clone(), Path(pdfs)).await.unwrap_err().status);
        }
        // Another person may use the same name
        make(&env, &ben, "PDFs", json!({ "ext": "pdf" })).await.unwrap();

        // Renamed, and changed to look for something else; its own name in another letter case is fine
        let Json(changed) = update_smart_folder(st(), amy.clone(), Path(pdfs), req(json!({ "name": "pdfs", "query": { "name": "zzz" } }))).await.unwrap();
        assert_eq!((changed.name.as_str(), changed.query.name.as_str(), changed.query.ext.as_deref()), ("pdfs", "zzz", None));
        assert!(names(&env, &amy, pdfs).await.is_empty());
        assert_eq!(update_smart_folder(st(), amy.clone(), Path(pdfs), req(json!({ "name": "home" }))).await.unwrap_err().status, StatusCode::CONFLICT);
        assert_eq!(update_smart_folder(st(), amy.clone(), Path(pdfs), req(json!({ "query": {} }))).await.unwrap_err().status, bad);
        let listed: Vec<String> = smart_folders(st(), amy.clone()).await.unwrap().0.into_iter().map(|s| s.name).collect();
        assert_eq!(listed, ["Home", "pdfs"]);

        // Deleted: only the smart folder goes
        let before = count_nodes(&env).await;
        let _ = update_smart_folder(st(), amy.clone(), Path(pdfs), req(json!({ "query": { "name": "a" } }))).await.unwrap();
        let _ = delete_smart_folder(st(), amy.clone(), Path(pdfs)).await.unwrap();
        assert_eq!(count_nodes(&env).await, before);
        assert_eq!(smart_folder(st(), amy.clone(), Path(pdfs)).await.unwrap_err().status, StatusCode::NOT_FOUND);
        assert_eq!(names(&env, &amy, home).await, ["a.pdf"]);

        // At most MAX_SMART each
        sqlx::query(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?)
             INSERT INTO smart_folders (owner_id, name, query, created_at, updated_at) SELECT ?, 'Saved ' || i, '{\"name\":\"a\"}', 0, 0 FROM n",
        )
        .bind(MAX_SMART - 1)
        .bind(amy.id)
        .execute(&env.st.db)
        .await
        .unwrap();
        assert_eq!(status(make(&env, &amy, "One more", json!({ "name": "a" })).await), bad);
    }

    #[tokio::test]
    async fn smart_folders_find_by_name_kind_type_size_date_place_and_tags_and_stay_current() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let st = || State(env.st.clone());
        let (docs, reports) = (env.folder(&amy, amy.root(), "Docs").await, env.folder(&amy, amy.root(), "Reports").await);
        let old = env.folder(&amy, &docs, "Old").await;
        let (jan, recent) = (1_768_000_000, now() - 3600);
        let report = env.file(&amy, &docs, "report.pdf").await;
        set(&env, &report, 2_000_000, jan).await;
        let notes = env.file(&amy, &docs, "notes.txt").await;
        set(&env, &notes, 10, recent).await;
        let old_report = env.file(&amy, &old, "old report.pdf").await;
        set(&env, &old_report, 50, 1_000_000_000).await;
        let photo = env.file(&amy, amy.root(), "photo.jpg").await;
        set(&env, &photo, 3_000, recent).await;
        let company = amy.shared_root.clone().unwrap();
        let plan = env.file(&amy, &company, "plan.pdf").await;
        set(&env, &plan, 500, recent).await;
        for id in [&docs, &reports, &old] {
            set(&env, id, 0, jan).await;
        }
        let sorted = |mut v: Vec<String>| {
            v.sort();
            v
        };

        // The name (a part of it, with or without the index), the kind and the type
        assert_eq!(found(&env, &amy, json!({ "name": "report" })).await, ["Reports", "old report.pdf", "report.pdf"]);
        assert_eq!(found(&env, &amy, json!({ "name": "re" })).await, ["Reports", "old report.pdf", "report.pdf"]);
        assert_eq!(found(&env, &amy, json!({ "name": "report", "kind": "file" })).await, ["old report.pdf", "report.pdf"]);
        assert_eq!(found(&env, &amy, json!({ "kind": "folder" })).await, ["Docs", "Old", "Reports"]);
        assert_eq!(found(&env, &amy, json!({ "ext": "pdf,jpg" })).await, ["old report.pdf", "photo.jpg", "plan.pdf", "report.pdf"]);
        // Size, and the date modified: between two dates, or in the last days
        assert_eq!(found(&env, &amy, json!({ "ext": "pdf", "min_size": 100, "max_size": 1_000 })).await, ["plan.pdf"]);
        assert_eq!(found(&env, &amy, json!({ "min_size": 1_000_000 })).await, ["report.pdf"]);
        assert_eq!(found(&env, &amy, json!({ "kind": "file", "modified_from": jan - 10, "modified_to": jan + 10 })).await, ["report.pdf"]);
        assert_eq!(found(&env, &amy, json!({ "modified_days": 1 })).await, ["notes.txt", "photo.jpg", "plan.pdf"]);
        assert_eq!(found(&env, &amy, json!({ "modified_to": 1_500_000_000 })).await, ["old report.pdf"]);
        // Where: a folder and its subfolders, a space, or everywhere
        let in_docs = json!({ "kind": "file", "scope": { "kind": "folder", "id": docs } });
        assert_eq!(found(&env, &amy, in_docs.clone()).await, ["notes.txt", "old report.pdf", "report.pdf"]);
        let space = env.drive_of(&company).await;
        assert_eq!(found(&env, &amy, json!({ "ext": "pdf", "scope": { "kind": "space", "id": space } })).await, ["plan.pdf"]);
        let mine = env.drive_of(amy.root()).await;
        assert_eq!(found(&env, &amy, json!({ "ext": "pdf", "scope": { "kind": "space", "id": mine } })).await, ["old report.pdf", "report.pdf"]);
        // Tags: all of them
        let (urgent, later) = (make_tag(&env, &amy, "Urgent").await, make_tag(&env, &amy, "Later").await);
        tag(&env, &amy, &[&notes, &photo, &reports], &[urgent], &[]).await.unwrap();
        tag(&env, &amy, &[&photo], &[later], &[]).await.unwrap();
        let tagged = make(&env, &amy, "Urgent", json!({ "tags": [urgent] })).await.unwrap();
        assert_eq!(names(&env, &amy, tagged).await, ["Reports", "notes.txt", "photo.jpg"]);
        assert_eq!(found(&env, &amy, json!({ "tags": [later, urgent] })).await, ["photo.jpg"]);
        assert_eq!(found(&env, &amy, json!({ "tags": [urgent], "kind": "file", "scope": { "kind": "folder", "id": docs } })).await, ["notes.txt"]);

        // In the order asked for, folders first
        let (by_size, ..) = page(&env, &amy, tagged, json!({ "sort": "size", "order": "desc" })).await.unwrap();
        assert_eq!(by_size, ["Reports", "photo.jpg", "notes.txt"]);
        // With where each item is, and the person's tags on it
        let Json(Listing::Page { items, .. }) = smart_items(st(), amy.clone(), Path(tagged), query(json!({}))).await.unwrap() else { panic!() };
        let listed = items.iter().find(|l| l.node.id == notes).unwrap();
        assert_eq!((&listed.location_path, &listed.node.tags), (&vec!["Docs".to_string()], &vec![urgent]));

        // Kept current: new items, renamed, moved out of the folder looked in, to the trash and back
        let docs_files = make(&env, &amy, "Docs files", in_docs).await.unwrap();
        let new = env.file(&amy, &old, "new.txt").await;
        let _ = crate::nodes::rename(st(), amy.clone(), Path(notes.clone()), req(json!({ "name": "renamed.txt" }))).await.unwrap();
        let _ = crate::nodes::move_nodes(st(), amy.clone(), req(json!({ "ids": [report], "dest_id": reports }))).await.unwrap();
        assert_eq!(sorted(names(&env, &amy, docs_files).await), ["new.txt", "old report.pdf", "renamed.txt"]);
        let _ = crate::nodes::trash(st(), amy.clone(), req(json!({ "ids": [old] }))).await.unwrap();
        assert_eq!(names(&env, &amy, docs_files).await, ["renamed.txt"]);
        let _ = crate::nodes::restore(st(), amy.clone(), req(json!({ "ids": [old] }))).await.unwrap();
        assert_eq!(names(&env, &amy, docs_files).await.len(), 3);
        // The folder it looks in gone: it lists nothing, and nothing fails
        let _ = crate::nodes::trash(st(), amy.clone(), req(json!({ "ids": [docs] }))).await.unwrap();
        assert!(names(&env, &amy, docs_files).await.is_empty());
        let Json(at) = smart_position(st(), amy.clone(), Path(docs_files), query(json!({ "item": new }))).await.unwrap();
        assert_eq!((at.position, at.total), (None, 0));
        // A tag deleted: nothing has it any more
        let _ = crate::tags::delete(st(), amy.clone(), Path(urgent)).await.unwrap();
        assert!(names(&env, &amy, tagged).await.is_empty());
    }

    #[tokio::test]
    async fn smart_folders_list_only_what_the_person_can_open_when_listed() {
        let env = testutil::env().await;
        let (amy, ben, admin) = (env.user("amy", true).await, env.user("ben", true).await, env.admin().await);
        let st = || State(env.st.clone());
        let plans = env.folder(&ben, ben.root(), "Plans").await;
        env.file(&ben, &plans, "plan.pdf").await;
        env.file(&ben, ben.root(), "private.pdf").await;
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        env.file(&amy, &docs, "secret.pdf").await;
        let company = amy.shared_root.clone().unwrap();
        env.file(&admin, &company, "everyone.pdf").await;

        let amys = make(&env, &amy, "PDFs", json!({ "ext": "pdf" })).await.unwrap();
        assert_eq!(names(&env, &amy, amys).await, ["everyone.pdf", "secret.pdf"]);
        // Shared with her: listed; no longer: gone, whenever the query was saved
        env.grant(&plans, &amy, "viewer").await;
        assert_eq!(names(&env, &amy, amys).await, ["everyone.pdf", "plan.pdf", "secret.pdf"]);
        let in_plans = make(&env, &amy, "Plans", json!({ "kind": "file", "scope": { "kind": "folder", "id": plans } })).await.unwrap();
        assert_eq!(names(&env, &amy, in_plans).await, ["plan.pdf"]);
        env.revoke(&plans, &amy).await;
        assert_eq!(names(&env, &amy, amys).await, ["everyone.pdf", "secret.pdf"]);
        assert!(names(&env, &amy, in_plans).await.is_empty());
        let Json(picked) = smart_select(st(), amy.clone(), Path(in_plans), req(json!({}))).await.unwrap();
        assert!(picked.ids.is_empty());
        // Saved for a space she was a member of: what she can't open in it isn't listed (the query is changed in the
        // database, as if she had saved it while she could)
        let bens_space = env.drive_of(ben.root()).await;
        let query_text = serde_json::to_string(&json!({ "ext": "pdf", "scope": { "kind": "space", "id": bens_space } })).unwrap();
        sqlx::query("UPDATE smart_folders SET query = ? WHERE id = ?").bind(query_text).bind(amys).execute(&env.st.db).await.unwrap();
        assert!(names(&env, &amy, amys).await.is_empty());

        // An administrator's: never what is in someone's own space
        let admins = make(&env, &admin, "PDFs", json!({ "ext": "pdf" })).await.unwrap();
        assert_eq!(names(&env, &admin, admins).await, ["everyone.pdf"]);
        let amys_space = env.drive_of(amy.root()).await;
        let status = make(&env, &admin, "Amy's", json!({ "ext": "pdf", "scope": { "kind": "space", "id": amys_space } })).await.unwrap_err().status;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // A share link gives a visitor nothing in their smart folders, even after visiting it
        let link = serde_json::from_value(json!({ "node_id": docs })).unwrap();
        let Json(link) = crate::shares::create(st(), amy.clone(), Json(link)).await.unwrap();
        let token = serde_json::to_value(&link).unwrap()["id"].as_str().unwrap().to_string();
        let visit = crate::shares::public_children(st(), Path((token, docs.clone())), query(json!({})), HeaderMap::new());
        assert_eq!(visit.await.unwrap().0.into_items().len(), 1);
        let bens = make(&env, &ben, "PDFs", json!({ "ext": "pdf" })).await.unwrap();
        assert_eq!(names(&env, &ben, bens).await, ["everyone.pdf", "plan.pdf", "private.pdf"]);
        let status = make(&env, &ben, "Amy's", json!({ "name": "x", "scope": { "kind": "folder", "id": docs } })).await.unwrap_err().status;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_large_smart_folder_lists_selects_and_finds_items_a_page_at_a_time() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let st = || State(env.st.clone());
        let big = env.folder(&amy, amy.root(), "Big").await;
        // 1,200 files that match, with sizes and times that tie, and some that don't match
        sqlx::query(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1300)
             INSERT INTO nodes (id, owner_id, parent_id, kind, name, size, mime, drive_id, created_at, updated_at)
             SELECT 'big' || i, ?1, ?2, 'file', 'File ' || i || CASE WHEN i <= 1200 THEN '.pdf' ELSE '.txt' END, i % 7, 'text/plain',
                    (SELECT drive_id FROM nodes WHERE id = ?2), i % 3, i % 5 FROM n",
        )
        .bind(amy.id)
        .bind(&big)
        .execute(&env.st.db)
        .await
        .unwrap();
        let pdfs = make(&env, &amy, "PDFs", json!({ "ext": "pdf" })).await.unwrap();

        for sort in ["name", "size", "updated", "created", "type"] {
            for order in ["asc", "desc"] {
                let whole = page(&env, &amy, pdfs, json!({ "sort": sort, "order": order })).await.unwrap().0;
                assert_eq!(whole.len(), 1200);
                // Page after page by `next`, and by position with how many there are: the same items in the same order
                let (mut by_next, mut after) = (Vec::new(), None::<String>);
                loop {
                    let (names, next, _) = page(&env, &amy, pdfs, json!({ "sort": sort, "order": order, "limit": 500, "after": after })).await.unwrap();
                    by_next.extend(names);
                    match next {
                        Some(n) => after = Some(n),
                        None => break,
                    }
                }
                assert_eq!(by_next, whole, "{sort} {order}");
                let mut by_offset = Vec::new();
                for offset in [0, 500, 1000] {
                    let (names, _, total) = page(&env, &amy, pdfs, json!({ "sort": sort, "order": order, "limit": 500, "offset": offset })).await.unwrap();
                    assert_eq!(total, Some(1200));
                    by_offset.extend(names);
                }
                assert_eq!(by_offset, whole, "{sort} {order}");
                // Where an item is
                let at = |item: &str| smart_position(st(), amy.clone(), Path(pdfs), query(json!({ "item": item, "sort": sort, "order": order })));
                let Json(p) = at("big700").await.unwrap();
                assert_eq!((p.position.map(|i| whole[i as usize].as_str()), p.total), (Some("File 700.pdf"), 1200));
                assert_eq!(at("big1250").await.unwrap().0.position, None, "doesn't match");
            }
        }

        // Everything but two, a batch at a time; and from one item to another
        let (mut ids, mut after, mut batches) = (Vec::new(), None::<String>, 0);
        loop {
            let Json(sel) = smart_select(st(), amy.clone(), Path(pdfs), req(json!({ "except": ["big1", "big2"], "after": after }))).await.unwrap();
            ids.extend(sel.ids);
            batches += 1;
            match sel.next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        assert_eq!((ids.len(), batches), (1198, 2));
        assert!(ids.iter().all(|id| id.trim_start_matches("big").parse::<i64>().unwrap() <= 1200));
        let whole = page(&env, &amy, pdfs, json!({})).await.unwrap().0;
        let (from, to) = (whole.iter().position(|n| n == "File 10.pdf").unwrap(), whole.iter().position(|n| n == "File 20.pdf").unwrap());
        let Json(sel) = smart_select(st(), amy.clone(), Path(pdfs), req(json!({ "from": "big10", "to": "big20" }))).await.unwrap();
        assert_eq!(sel.ids.len(), to - from + 1);
        // An end that isn't in the smart folder
        let err = smart_select(st(), amy.clone(), Path(pdfs), req(json!({ "from": "big1250" }))).await.unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
    }
}
