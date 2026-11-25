//! Single-user mode (`THIRTYFILE_SINGLE_USER`): a file manager for one person in a sandbox or another trusted place.
//!
//! The mode is explicit and changes nothing for a normal server. When it is on:
//!
//! - nobody signs in: every request is the one local user, who has one space showing the folder of
//!   `THIRTYFILE_FILES` (a folder space, folders/) as "My files". The user is an ordinary user (not an administrator)
//!   who may not share, with no password and no other way in
//! - the server listens on the loopback address unless `THIRTYFILE_ADDR` says otherwise (`app::startup`)
//! - what needs more than one person is off on the server, not only hidden: accounts, groups, permissions, sharing,
//!   the Control panel, WebDAV, S3, SFTP, FTP and monitoring. `allows` is the list of what is left, and `guard` answers
//!   everything else with 404 (these features don't exist here)
//! - with nobody to sign in, other websites would be the way in, so `guard` also refuses requests that come from
//!   another origin (browsers say so in `Sec-Fetch-Site`) and requests for any name but this computer's
//!   (`localhost`, `127.0.0.1`, `[::1]`, and `THIRTYFILE_ALLOWED_HOSTS`): a page that makes its own name point at
//!   the loopback address (DNS rebinding) is refused. Mutating requests are checked for their origin as always
//!   (`app::middleware::same_origin`).
//!
//! The data folder belongs to one mode: single-user data can't be started as a normal server, and a normal server's
//! data can't be started in single-user mode, so nobody gets in without signing in by mistake.

use std::path::Path;

use axum::{
    extract::{Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sqlx::SqlitePool;

use crate::{
    db::{add_grant, begin_write, create_drive},
    error::{AppError, AppResult},
    state::AppState,
    users::{NewUser, create_user},
};

/// `users.source` of the local user: no sign-in provider and no password
const SOURCE: &str = "single_user";
const USERNAME: &str = "local";

/// What single-user mode keeps (a part of `AppState`; a normal server has none)
pub struct Memory {
    pub user_id: i64,
    /// Names the server answers to besides this computer's own, lower case, without port
    allowed_hosts: Vec<String>,
}

impl Memory {
    pub fn new(user_id: i64, allowed_hosts: &str) -> Memory {
        let allowed_hosts = allowed_hosts.split(',').map(|h| h.trim().trim_matches(['[', ']']).to_ascii_lowercase()).filter(|h| !h.is_empty()).collect();
        Memory { user_id, allowed_hosts }
    }
}

/// Before a normal server starts: single-user data (a user nobody can sign in as) isn't for it
pub async fn refuse_single_user_data(db: &SqlitePool) -> AppResult<()> {
    let (found,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM users WHERE source = ?)").bind(SOURCE).fetch_one(db).await?;
    if found {
        return Err(AppError::bad_request(
            "This data folder belongs to single-user mode (THIRTYFILE_SINGLE_USER). Set it again, or use another data folder for a normal server.",
        ));
    }
    Ok(())
}

/// The folders `THIRTYFILE_FILES` names: a single full path (shown as "My files"), or a list of `Name=/full/path`
/// entries separated by commas, each shown as a space of its own with that name
#[derive(Debug, PartialEq)]
pub enum Folders {
    One(String),
    Named(Vec<(String, String)>),
}

/// Reads `THIRTYFILE_FILES`. A path without a name has no `=` before its first folder separator, so a folder with an `=`
/// in its name still works as the only one.
pub fn parse_folders(spec: &str) -> AppResult<Folders> {
    let spec = spec.trim();
    let named = |entry: &str| entry.split_once('=').filter(|(name, _)| !name.contains(['/', '\\'])).map(|(n, p)| (n.trim().to_string(), p.trim().to_string()));
    if !spec.split(',').any(|e| named(e).is_some()) {
        return Ok(Folders::One(spec.to_string()));
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let Some((name, path)) = named(entry) else {
            return Err(AppError::bad_request(format!("Give every folder in THIRTYFILE_FILES a name, as in Workspace=/workspace: \"{entry}\" has none")));
        };
        let name = crate::util::validate_name(&name)?;
        if path.is_empty() {
            return Err(AppError::bad_request(format!("The folder named \"{name}\" in THIRTYFILE_FILES has no path")));
        }
        if out.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name)) {
            return Err(AppError::bad_request(format!("THIRTYFILE_FILES names \"{name}\" twice")));
        }
        out.push((name, path));
    }
    Ok(Folders::Named(out))
}

/// Before the server starts in single-user mode: the local user, and their spaces showing the folders of `files`
/// (`parse_folders`). Returns the user's id. Choosing other folders later shows those instead (a folder no longer
/// named is left alone, and shown again if it is named again).
pub async fn prepare(db: &SqlitePool, data_dir: &Path, storage_dir: &Path, files: &str) -> AppResult<i64> {
    let wanted = match parse_folders(files)? {
        Folders::One(path) => vec![(None, path)],
        Folders::Named(list) => list.into_iter().map(|(name, path)| (Some(name), path)).collect(),
    };
    let mut roots: Vec<(Option<String>, String)> = Vec::new();
    for (name, path) in wanted {
        let root = crate::folders::check_source_in(data_dir, storage_dir, &path)?;
        for (other, taken) in &roots {
            if Path::new(&root).starts_with(taken) || Path::new(taken).starts_with(&root) {
                let shown = |n: &Option<String>| n.as_deref().map_or_else(|| "a folder".to_string(), |n| format!("\"{n}\""));
                let (first, second) = (shown(other), shown(&name));
                return Err(AppError::bad_request(format!(
                    "THIRTYFILE_FILES: {first} and {second} are the same folder or one is inside the other. Each name needs a folder of its own."
                )));
            }
        }
        roots.push((name, root));
    }
    let existing: Option<(i64,)> = sqlx::query_as("SELECT id FROM users WHERE source = ?").bind(SOURCE).fetch_optional(db).await?;
    let (others,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE source <> ?").bind(SOURCE).fetch_one(db).await?;
    if others > 0 {
        return Err(AppError::bad_request(
            "This data folder already has accounts. Single-user mode needs a new data folder (THIRTYFILE_DATA); use another one for it.",
        ));
    }
    let mut tx = begin_write(db).await?;
    let user_id = match existing {
        Some((id,)) => id,
        None => {
            create_user(
                &mut tx,
                NewUser {
                    username: USERNAME,
                    // Not a hash: no password matches it (and there is no sign-in)
                    password_hash: crate::auth::NO_PASSWORD,
                    role: crate::auth::UserRole::User,
                    guest: false,
                    can_write: true,
                    can_delete: true,
                    can_share: false,
                    quota_bytes: 0,
                    source: SOURCE,
                    provisioned_by: None,
                    personal_space: None,
                    space_folders: None,
                },
            )
            .await?;
            let (id,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE source = ?").bind(SOURCE).fetch_one(&mut *tx).await?;
            id
        }
    };
    match roots.as_slice() {
        // One folder, shown as "My files"
        [(None, root)] => show_as_my_files(&mut tx, user_id, root).await?,
        _ => show_by_name(&mut tx, user_id, &roots).await?,
    }
    tx.commit().await?;
    Ok(user_id)
}

/// "My files" shows `root`; the spaces a list of names made are put away
async fn show_as_my_files(tx: &mut sqlx::SqliteConnection, user_id: i64, root: &str) -> AppResult<()> {
    let space: Option<(String, String, Option<String>)> =
        sqlx::query_as("SELECT id, root_id, source_path FROM drives WHERE kind = 'personal' AND owner_id = ?").bind(user_id).fetch_optional(&mut *tx).await?;
    let (drive, root_id) = match space {
        Some((drive, root_id, shown)) => {
            // The folder chosen now: a space that showed another one shows this one
            if shown.as_deref() != Some(root) {
                crate::folders::set_up(tx, &drive, &root_id, root, None).await?;
                sqlx::query("UPDATE drives SET last_scan_at = NULL, scan_report = NULL WHERE id = ?").bind(&drive).execute(&mut *tx).await?;
            }
            (drive, root_id)
        }
        None => {
            let location = crate::locations::default_location(tx).await?;
            let (drive, root_id) = create_drive(tx, crate::personal::NAME, "personal", user_id, 0, &location).await?;
            add_grant(tx, &root_id, "user", user_id, "owner", Some(user_id), None).await?;
            crate::folders::set_up(tx, &drive, &root_id, root, None).await?;
            (drive, root_id)
        }
    };
    sqlx::query("UPDATE drives SET disabled = 0 WHERE id = ?").bind(&drive).execute(&mut *tx).await?;
    sqlx::query("UPDATE users SET root_id = ? WHERE id = ?").bind(&root_id).bind(user_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE drives SET disabled = 1 WHERE kind = 'team' AND owner_id = ?").bind(user_id).execute(&mut *tx).await?;
    Ok(())
}

/// Each folder is a space of its own with its name, and there is no "My files": a space of an earlier start that isn't
/// named now is put away (not deleted), and comes back when it is named again
async fn show_by_name(tx: &mut sqlx::SqliteConnection, user_id: i64, roots: &[(Option<String>, String)]) -> AppResult<()> {
    sqlx::query("UPDATE drives SET disabled = 1 WHERE owner_id = ? AND kind IN ('personal', 'team')").bind(user_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE users SET root_id = NULL WHERE id = ?").bind(user_id).execute(&mut *tx).await?;
    for (name, root) in roots {
        let name = name.as_deref().unwrap_or_default();
        let space: Option<(String,)> = sqlx::query_as("SELECT id FROM drives WHERE kind = 'team' AND owner_id = ? AND source_path = ?")
            .bind(user_id)
            .bind(root)
            .fetch_optional(&mut *tx)
            .await?;
        match space {
            Some((drive,)) => {
                sqlx::query("UPDATE drives SET name = ?, disabled = 0 WHERE id = ?").bind(name).bind(&drive).execute(&mut *tx).await?;
            }
            None => {
                let location = crate::locations::default_location(tx).await?;
                let (drive, root_id) = create_drive(tx, name, "team", user_id, 0, &location).await?;
                add_grant(tx, &root_id, "user", user_id, "owner", Some(user_id), None).await?;
                crate::folders::set_up(tx, &drive, &root_id, root, None).await?;
            }
        }
    }
    Ok(())
}

/// The shared space ("All files") exists in every data folder and has nothing to do here: it is switched off (once
/// it exists, which is after `prepare`)
pub async fn switch_off_shared_space(db: &SqlitePool) -> AppResult<()> {
    sqlx::query("UPDATE drives SET disabled = 1 WHERE kind = 'company'").execute(db).await?;
    Ok(())
}

/// The paths of the API that single-user mode keeps (below `/api`): files and folders, uploads, downloads, search,
/// tags, the trash, favourites, the user's own display settings, and the page's own branding
pub fn allows(method: &Method, path: &str) -> bool {
    let path = path.trim_start_matches('/');
    let mut parts = path.split('/');
    let (first, second, third) = (parts.next().unwrap_or(""), parts.next(), parts.next());
    match first {
        "files" | "uploads" | "folders" | "trash" | "search" | "recent" | "favorites" | "tags" | "smart-folders" | "download" | "archive" | "jobs" | "health"
        | "client-errors" | "branding" | "branding.css" => true,
        // Items: everything but who may do what to them
        "nodes" => !matches!(third, Some("access" | "restriction" | "retention" | "activity")),
        // The account's own settings that make sense without an account
        "auth" => matches!((second, method), (Some("me"), &Method::GET) | (Some("language" | "style"), &Method::PUT)),
        // Spaces: the list (the interface asks for it); nothing is created or changed
        "drives" => *method == Method::GET && (second.is_none() || third == Some("analytics")),
        _ => false,
    }
}

/// Whether the request's own name (`Host`) is one the server answers to
fn host_allowed(m: &Memory, host: &str) -> bool {
    // `host` or `host:port`, `[::1]` or `[::1]:port`
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => host.rsplit_once(':').map_or(host, |(name, port)| if port.chars().all(|c| c.is_ascii_digit()) { name } else { host }),
    }
    .to_ascii_lowercase();
    name == "localhost" || name.ends_with(".localhost") || name == "127.0.0.1" || name == "::1" || m.allowed_hosts.contains(&name)
}

/// Refuses what single-user mode doesn't do, and what another website could make a browser do
pub async fn guard(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let Some(m) = st.try_part::<Memory>() else { return next.run(req).await };
    let h = req.headers();
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !host_allowed(m, host) {
        return (StatusCode::FORBIDDEN, "unknown host name: add it to THIRTYFILE_ALLOWED_HOSTS").into_response();
    }
    // Browsers say where a request comes from; only the page itself (or the address bar) may ask for anything
    if h.get("sec-fetch-site").and_then(|v| v.to_str().ok()).is_some_and(|site| !matches!(site, "same-origin" | "none")) {
        return (StatusCode::FORBIDDEN, "cross-origin request blocked").into_response();
    }
    let path = req.uri().path();
    let open = match path.strip_prefix("/api") {
        Some(api) => allows(req.method(), api),
        // The web interface (WebDAV and S3 clients use other methods, and `/dav` and `/metrics` are theirs)
        None => matches!(*req.method(), Method::GET | Method::HEAD) && !path.starts_with("/dav") && path != "/metrics",
    };
    if !open {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_one_person_needs_is_allowed() {
        for (method, path) in [
            (Method::GET, "/nodes/abc/children"),
            (Method::POST, "/nodes/move"),
            (Method::PUT, "/files/abc/content"),
            (Method::POST, "/uploads"),
            (Method::GET, "/download/xyz"),
            (Method::GET, "/search"),
            (Method::GET, "/auth/me"),
            (Method::PUT, "/auth/language"),
            (Method::GET, "/drives"),
            (Method::GET, "/drives/abc/analytics"),
            (Method::GET, "/branding"),
        ] {
            assert!(allows(&method, path), "{method} {path}");
        }
        for (method, path) in [
            (Method::GET, "/admin/users"),
            (Method::GET, "/admin/settings"),
            (Method::POST, "/auth/login"),
            (Method::PUT, "/auth/password"),
            (Method::GET, "/auth/sessions"),
            (Method::POST, "/auth/app-passwords"),
            (Method::GET, "/shares"),
            (Method::POST, "/shares"),
            (Method::GET, "/public/shares/abc"),
            (Method::GET, "/shared-with-me"),
            (Method::GET, "/nodes/abc/access"),
            (Method::PUT, "/nodes/abc/restriction"),
            (Method::GET, "/nodes/abc/activity"),
            (Method::GET, "/directory"),
            (Method::GET, "/activity"),
            (Method::POST, "/drives"),
            (Method::DELETE, "/drives/abc"),
            (Method::PUT, "/grants/abc"),
            (Method::GET, "/s3/buckets"),
            (Method::GET, "/ftp/folders"),
            (Method::GET, "/notifications"),
        ] {
            assert!(!allows(&method, path), "{method} {path}");
        }
    }

    #[test]
    fn only_this_computers_names_are_answered() {
        let m = Memory::new(1, "files.example.com, [::2]");
        for host in [
            "localhost",
            "localhost:8080",
            "127.0.0.1:18080",
            "[::1]:8080",
            "[::1]",
            "App.LOCALHOST:3000",
            "files.example.com",
            "Files.Example.com:8443",
            "[::2]:80",
        ] {
            assert!(host_allowed(&m, host), "{host}");
        }
        for host in ["", "evil.example.com", "evil.example.com:8080", "localhost.evil.example.com", "127.0.0.1.evil.example.com", "192.168.1.5:8080", "[::3]"] {
            assert!(!host_allowed(&m, host), "{host}");
        }
    }

    #[test]
    fn the_folders_are_one_path_or_a_list_of_named_ones() {
        assert_eq!(parse_folders(" /srv/work ").unwrap(), Folders::One("/srv/work".into()));
        // An `=` in a folder's name isn't a name
        assert_eq!(parse_folders("/srv/a=b").unwrap(), Folders::One("/srv/a=b".into()));
        assert_eq!(parse_folders(r"C:\work=1").unwrap(), Folders::One(r"C:\work=1".into()));
        assert_eq!(
            parse_folders("Workspace=/workspace, Home = /home/sandbox,").unwrap(),
            Folders::Named(vec![("Workspace".into(), "/workspace".into()), ("Home".into(), "/home/sandbox".into())])
        );
        assert_eq!(parse_folders("Work=/srv/a=b").unwrap(), Folders::Named(vec![("Work".into(), "/srv/a=b".into())]));
        for bad in ["Work=/a,/b", "Work=", "Work=/a,work=/b", "=/a", "A:B=/a"] {
            assert!(parse_folders(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn several_named_folders_are_spaces_of_their_own() {
        let (work, home) = (files_folder(), files_folder());
        let spec = format!("Workspace={},Home={}", work.display(), home.display());
        let env = crate::testutil::single_user_env(Path::new(&spec)).await;
        let app = crate::app::routes::router(env.st.clone());
        let (_, me) = call(&app, Method::GET, "/api/auth/me", &[], None).await;
        assert!(me["root_id"].is_null(), "{me}");
        let (status, drives) = call(&app, Method::GET, "/api/drives", &[], None).await;
        assert_eq!(status, StatusCode::OK, "{drives}");
        let mut names: Vec<&str> = drives.as_array().unwrap().iter().filter_map(|d| d["name"].as_str()).collect();
        names.sort();
        assert_eq!(names, ["Home", "Workspace"], "{drives}");
        // Each shows its own folder, and a change is made in that folder
        let workspace = drives.as_array().unwrap().iter().find(|d| d["name"] == "Workspace").unwrap();
        let root = workspace["root_id"].as_str().unwrap().to_string();
        let (drive,): (String,) = sqlx::query_as("SELECT id FROM drives WHERE root_id = ?").bind(&root).fetch_one(&env.st.db).await.unwrap();
        crate::folders::scan(&env.st, &drive).await.unwrap();
        let made = serde_json::json!({ "parent_id": root, "name": "Made" });
        let (status, made) = call(&app, Method::POST, "/api/folders", &[("origin", "http://localhost:8080")], Some(made)).await;
        assert_eq!(status, StatusCode::OK, "{made}");
        assert!(work.join("Made").is_dir() && !home.join("Made").exists());
        // Starting again with one name changed, and one folder left out: that one is put away, not deleted
        let again = format!("Code={}", work.display());
        let id = prepare(&env.st.db, &env.dir, &env.dir.join("blobs"), &again).await.unwrap();
        let shown: Vec<(String,)> = sqlx::query_as("SELECT name FROM drives WHERE owner_id = ? AND kind <> 'company' AND disabled = 0 ORDER BY name")
            .bind(id)
            .fetch_all(&env.st.db)
            .await
            .unwrap();
        assert_eq!(shown, [("Code".to_string(),)]);
        let (put_away,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM drives WHERE owner_id = ? AND kind = 'team' AND disabled = 1").bind(id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(put_away, 1);
        // Back to one folder: "My files", and the named spaces go away from the list
        prepare(&env.st.db, &env.dir, &env.dir.join("blobs"), &work.to_string_lossy()).await.unwrap();
        let (_, me) = call(&app, Method::GET, "/api/auth/me", &[], None).await;
        assert!(me["root_id"].is_string(), "{me}");
        let (_, drives) = call(&app, Method::GET, "/api/drives", &[], None).await;
        assert_eq!(drives.as_array().unwrap().len(), 1, "{drives}");
        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn folders_that_overlap_are_refused() {
        let work = files_folder();
        let env = crate::testutil::single_user_env(&work).await;
        let inside = work.join("Docs");
        for spec in [
            format!("A={},B={}", work.display(), inside.display()),
            format!("A={},B={}", inside.display(), work.display()),
            format!("A={},B={}", work.display(), work.display()),
        ] {
            let err = prepare(&env.st.db, &env.dir, &env.dir.join("blobs"), &spec).await.unwrap_err();
            assert!(err.message.contains("inside the other"), "{spec}: {}", err.message);
        }
        let _ = std::fs::remove_dir_all(&work);
    }

    use axum::{
        body::Body,
        http::{Request, header},
    };
    use serde_json::Value;
    use tower::ServiceExt;

    async fn call(app: &axum::Router, method: Method, uri: &str, headers: &[(&str, &str)], body: Option<Value>) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(uri).header(header::HOST, "localhost:8080");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        req = req.extension(axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 5000))));
        let req = match body {
            Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        };
        let res = app.clone().oneshot(req.unwrap()).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn files_folder() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("thirtyfile-files-{}", crate::util::new_id()));
        std::fs::create_dir_all(dir.join("Docs")).unwrap();
        std::fs::write(dir.join("Docs").join("note.txt"), b"hello").unwrap();
        std::fs::write(dir.join("top.txt"), b"top").unwrap();
        // Files changed in the last seconds are left for a later scan
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for f in [dir.join("Docs").join("note.txt"), dir.join("top.txt")] {
            std::fs::File::options().write(true).open(f).unwrap().set_modified(old).unwrap();
        }
        dir
    }

    #[tokio::test]
    async fn anyone_reaching_the_server_is_the_local_user_with_the_folder_as_their_files() {
        let files = files_folder();
        let env = crate::testutil::single_user_env(&files).await;
        let app = crate::app::routes::router(env.st.clone());
        let (status, me) = call(&app, Method::GET, "/api/auth/me", &[], None).await;
        assert_eq!(status, StatusCode::OK, "{me}");
        assert_eq!((me["single_user"].clone(), me["role"].clone(), me["can_share"].clone()), (true.into(), "user".into(), false.into()), "{me}");
        assert!(me["shared_root"].is_null(), "{me}");
        // A cookie from an earlier life makes no difference, and nobody can sign in
        let (status, _) = call(&app, Method::GET, "/api/auth/me", &[("cookie", "tf_session=abc")], None).await;
        assert_eq!(status, StatusCode::OK);
        let login = serde_json::json!({ "username": "local", "password": "" });
        assert_eq!(call(&app, Method::POST, "/api/auth/login", &[], Some(login)).await.0, StatusCode::NOT_FOUND);
        // The folder's files are the space's files
        let root = me["root_id"].as_str().unwrap().to_string();
        let (drive,): (String,) = sqlx::query_as("SELECT id FROM drives WHERE root_id = ?").bind(&root).fetch_one(&env.st.db).await.unwrap();
        crate::folders::scan(&env.st, &drive).await.unwrap();
        let (status, list) = call(&app, Method::GET, &format!("/api/nodes/{root}/children"), &[], None).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        let items = list["items"].as_array().or(list.as_array()).unwrap();
        let names: Vec<&str> = items.iter().filter_map(|n| n["name"].as_str()).collect();
        assert_eq!(names, ["Docs", "top.txt"], "{list}");
        // Making a folder makes it on the disk
        let made = serde_json::json!({ "parent_id": root, "name": "Made" });
        let (status, made) = call(&app, Method::POST, "/api/folders", &[("origin", "http://localhost:8080")], Some(made)).await;
        assert_eq!(status, StatusCode::OK, "{made}");
        assert!(files.join("Made").is_dir());
        let _ = std::fs::remove_dir_all(&files);
    }

    #[tokio::test]
    async fn what_needs_more_than_one_person_isnt_there() {
        let files = files_folder();
        let env = crate::testutil::single_user_env(&files).await;
        let app = crate::app::routes::router(env.st.clone());
        let (_, me) = call(&app, Method::GET, "/api/auth/me", &[], None).await;
        let root = me["root_id"].as_str().unwrap().to_string();
        for (method, uri) in [
            (Method::GET, "/api/admin/users".to_string()),
            (Method::GET, "/api/admin/settings".to_string()),
            (Method::GET, "/api/admin/groups".to_string()),
            (Method::GET, "/api/shares".to_string()),
            (Method::POST, "/api/shares".to_string()),
            (Method::GET, "/api/shared-with-me".to_string()),
            (Method::GET, format!("/api/nodes/{root}/access")),
            (Method::GET, "/api/directory".to_string()),
            (Method::GET, "/api/auth/sessions".to_string()),
            (Method::GET, "/api/s3/buckets".to_string()),
            (Method::GET, "/metrics".to_string()),
            (Method::GET, "/dav/".to_string()),
            (Method::from_bytes(b"PROPFIND").unwrap(), "/".to_string()),
        ] {
            let (status, _) = call(&app, method.clone(), &uri, &[], None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
        }
        let _ = std::fs::remove_dir_all(&files);
    }

    #[tokio::test]
    async fn other_websites_and_other_names_are_refused() {
        let files = files_folder();
        let env = crate::testutil::single_user_env(&files).await;
        let app = crate::app::routes::router(env.st.clone());
        for headers in [&[("sec-fetch-site", "cross-site")][..], &[("sec-fetch-site", "same-site")]] {
            assert_eq!(call(&app, Method::GET, "/api/auth/me", headers, None).await.0, StatusCode::FORBIDDEN, "{headers:?}");
        }
        for headers in [&[("sec-fetch-site", "same-origin")][..], &[("sec-fetch-site", "none")]] {
            assert_eq!(call(&app, Method::GET, "/api/auth/me", headers, None).await.0, StatusCode::OK, "{headers:?}");
        }
        // A name that points at this computer (DNS rebinding) isn't this computer's
        let rebinding = Request::builder()
            .uri("/api/auth/me")
            .header(header::HOST, "rebind.example.com:8080")
            .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 5000))))
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.clone().oneshot(rebinding).await.unwrap().status(), StatusCode::FORBIDDEN);
        // Changes from another origin are refused, as in every mode
        let made = serde_json::json!({ "parent_id": "x", "name": "Evil" });
        assert_eq!(call(&app, Method::POST, "/api/folders", &[("origin", "http://evil.example.com")], Some(made)).await.0, StatusCode::FORBIDDEN);
        assert!(!files.join("Evil").exists());
        let _ = std::fs::remove_dir_all(&files);
    }

    #[tokio::test]
    async fn the_data_folder_belongs_to_one_mode() {
        let files = files_folder();
        let env = crate::testutil::single_user_env(&files).await;
        // A normal server doesn't start on single-user data, and the other way round
        assert!(refuse_single_user_data(&env.st.db).await.is_err());
        let normal = crate::testutil::env().await;
        assert!(refuse_single_user_data(&normal.st.db).await.is_ok());
        let err = prepare(&normal.st.db, &normal.dir, &normal.dir.join("blobs"), &files.to_string_lossy()).await.unwrap_err();
        assert!(err.message.contains("already has accounts"), "{}", err.message);
        // Folders that can't be the files
        let missing = files.join("nothing-here").to_string_lossy().into_owned();
        assert!(prepare(&env.st.db, &env.dir, &env.dir.join("blobs"), &missing).await.is_err());
        assert!(prepare(&env.st.db, &env.dir, &env.dir.join("blobs"), &env.dir.to_string_lossy()).await.is_err());
        // Starting again with another folder shows that one in the same space
        let other = files_folder();
        let id = prepare(&env.st.db, &env.dir, &env.dir.join("blobs"), &other.to_string_lossy()).await.unwrap();
        let (shown,): (String,) =
            sqlx::query_as("SELECT source_path FROM drives WHERE owner_id = ? AND kind = 'personal'").bind(id).fetch_one(&env.st.db).await.unwrap();
        let expected = crate::folders::check_source_in(&env.dir, &env.dir.join("blobs"), &other.to_string_lossy()).unwrap();
        assert_eq!(shown, expected);
        let _ = std::fs::remove_dir_all(&files);
        let _ = std::fs::remove_dir_all(&other);
    }
}
