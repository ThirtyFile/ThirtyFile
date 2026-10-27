use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde_json::{Value, json};

use super::{compute::*, *};
use crate::testutil::{self, TestEnv};

const BIG: [u8; 3000] = [7; 3000];

async fn compute_for(env: &TestEnv, user: &User, drive: &str) -> Snapshot {
    let mut conn = env.st.db.acquire().await.unwrap();
    let drive = allowed(&mut conn, user, drive).await.unwrap();
    compute(&env.st, &mut conn, user, &drive).await.unwrap()
}

async fn set(env: &TestEnv, sql: &str, args: &[&str]) {
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.to_string()));
    for a in args {
        q = q.bind(*a);
    }
    q.execute(&env.st.db).await.unwrap();
}

#[test]
fn files_are_sorted_into_the_kinds_the_overview_shows() {
    for (name, mime, kind) in [
        ("photo.JPG", "image/jpeg", "pictures"),
        ("scan", "image/png", "pictures"),
        ("clip.mov", "video/quicktime", "video"),
        ("song.flac", "", "audio"),
        ("backup.tar", "application/x-tar", "archives"),
        ("a.zip", "application/zip", "archives"),
        ("report.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", "documents"),
        ("notes.txt", "text/plain", "documents"),
        ("paper.pdf", "application/pdf", "documents"),
        ("data.bin", "application/octet-stream", "other"),
        ("noextension", "", "other"),
    ] {
        assert_eq!(kind_of(name, mime), kind, "{name}");
    }
}

#[tokio::test]
async fn the_figures_of_a_space_add_up() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let a = env.folder(&admin, admin.root(), "Projects").await;
    let b = env.folder(&admin, &a, "2024").await;
    let empty = env.folder(&admin, admin.root(), "Empty").await;
    let _ = empty;
    let photo = env.stored_file(&admin, &b, "photo.jpg", &[1u8; 5000]).await;
    let report = env.stored_file(&admin, &a, "report.pdf", &[2u8; 2000]).await;
    let copy1 = env.stored_file(&admin, admin.root(), "copy.bin", &BIG).await;
    let copy2 = env.stored_file(&admin, &a, "copy-2.bin", &BIG).await;
    let copy3 = env.stored_file(&admin, &b, "copy-3.bin", &BIG).await;
    let trashed = env.stored_file(&admin, &a, "deleted.txt", &[9u8; 4000]).await;
    let t = crate::util::now();
    set(&env, "UPDATE nodes SET trashed_at = ?1 WHERE id = ?2", &[&t.to_string(), &trashed]).await;
    set(&env, "UPDATE nodes SET updated_at = ?1 WHERE id = ?2", &[&(t - 2 * 365 * 86400).to_string(), &report]).await;
    // Two earlier versions, one older than a year
    for (id, size, created) in [("v1", 700, t - 3 * 365 * 86400), ("v2", 300, t - 86400)] {
        sqlx::query("INSERT INTO node_versions (id, node_id, blob_hash, size, author_name, modified_at, created_at) VALUES (?, ?, NULL, ?, '', 1, ?)")
            .bind(id)
            .bind(&photo)
            .bind(size)
            .bind(created)
            .execute(&env.st.db)
            .await
            .unwrap();
    }
    let drive = env.drive_of(&photo).await;
    let s = compute_for(&env, &admin, &drive).await;

    assert_eq!((s.files, s.folders), (5, 4), "the root, Projects, 2024 and Empty");
    assert_eq!(s.live_bytes, 5000 + 2000 + 3 * 3000);
    let kind = |k: &str| s.kinds.iter().find(|f| f.kind == k).map(|f| (f.files, f.bytes)).unwrap();
    assert_eq!((kind("pictures"), kind("documents"), kind("other"), kind("video")), ((1, 5000), (1, 2000), (3, 9000), (0, 0)));
    assert_eq!(s.kinds.iter().map(|k| k.kind.as_str()).collect::<Vec<_>>(), ["pictures", "video", "audio", "documents", "archives", "other"]);
    assert_eq!((s.trash_bytes, s.trash_files), (4000, 1), "the trash is apart from what is live");
    assert_eq!((s.version_bytes, s.old_version_bytes), (1000, 700));
    assert_eq!((s.stale_bytes, s.stale_files), (2000, 1), "only the report is older than a year");

    // The largest first, with where they are and whose they are
    let names: Vec<&str> = s.largest.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names[0], "photo.jpg");
    assert_eq!(s.largest[0].folder, "/Projects/2024");
    assert_eq!(s.largest[0].owner, "admin");
    assert_eq!(s.largest.iter().map(|f| f.size).collect::<Vec<_>>(), [5000, 3000, 3000, 3000, 2000]);
    assert_eq!(s.largest.iter().find(|f| f.name == "copy.bin").unwrap().folder, "/");

    // Three files with one content: copies that take quota, not disk
    assert_eq!(s.duplicates.groups.len(), 1);
    let g = &s.duplicates.groups[0];
    assert_eq!((g.copies, g.size, g.wasted), (3, 3000, 6000));
    assert_eq!(s.duplicates.wasted_bytes, 6000);
    let mut ids: Vec<&str> = g.files.iter().map(|f| f.id.as_str()).collect();
    ids.sort();
    let mut want = vec![copy1.as_str(), copy2.as_str(), copy3.as_str()];
    want.sort();
    assert_eq!(ids, want);
    assert!(!s.duplicates.partial && !s.folder_space);

    // Folders by size, with the newest change below them
    let rows: Vec<(&str, i64, i64)> = s.folders_by_size.iter().map(|f| (f.path.as_str(), f.bytes, f.files)).collect();
    assert_eq!(rows, [("/Projects", 5000 + 2000 + 3000 + 3000, 4), ("/Projects/2024", 5000 + 3000, 2)], "the empty folder and the root are not listed");
    assert_eq!(s.folders_by_size[0].newest, t, "the newest of what is below");
    assert!(s.folders_by_size[0].newest >= s.folders_by_size[1].newest);

    // The tree: sizes below each folder, and the files directly in the root as one block
    assert_eq!(s.tree.bytes, s.live_bytes);
    let projects = s.tree.children.iter().find(|c| c.name == "Projects").unwrap();
    assert_eq!((projects.bytes, projects.files), (13000, 4));
    assert!(projects.children.iter().any(|c| c.name == "2024" && c.bytes == 8000));
    assert!(projects.children.iter().any(|c| c.kind == "files" && c.bytes == 5000), "report.pdf and copy-2 are directly in Projects");
    assert!(!s.tree.children.iter().any(|c| c.name == "Empty"), "a folder holding nothing is not a block");
    assert!(s.tree.children.iter().any(|c| c.kind == "files" && c.bytes == 3000), "copy.bin is in the root");
}

#[tokio::test]
async fn only_the_largest_hundred_are_kept_whatever_the_size_of_the_space() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let folder = env.folder(&admin, admin.root(), "Many").await;
    for i in 1..=130 {
        let id = env.file(&admin, &folder, &format!("f{i:03}.bin")).await;
        set(&env, "UPDATE nodes SET size = ?1 WHERE id = ?2", &[&(i * 10).to_string(), &id]).await;
    }
    let drive = env.drive_of(&folder).await;
    let s = compute_for(&env, &admin, &drive).await;
    assert_eq!((s.files, s.largest.len()), (130, LARGEST));
    assert_eq!(s.largest[0].size, 1300);
    assert_eq!(s.largest[99].size, 310, "the 100th largest of 130");
    assert!(s.largest.windows(2).all(|w| w[0].size >= w[1].size));
}

#[tokio::test]
async fn the_treemap_shows_the_largest_folders_and_one_block_for_the_rest() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let top = env.folder(&admin, admin.root(), "Top").await;
    for i in 1..=25 {
        let f = env.folder(&admin, &top, &format!("d{i:02}")).await;
        let file = env.file(&admin, &f, "x.bin").await;
        set(&env, "UPDATE nodes SET size = ?1 WHERE id = ?2", &[&(i * 100).to_string(), &file]).await;
    }
    // Deeper than the tree goes
    let mut at = top.clone();
    for name in ["l1", "l2", "l3", "l4", "l5"] {
        at = env.folder(&admin, &at, name).await;
    }
    let deep = env.file(&admin, &at, "deep.bin").await;
    set(&env, "UPDATE nodes SET size = 1 WHERE id = ?1", &[&deep]).await;
    let drive = env.drive_of(&top).await;
    let s = compute_for(&env, &admin, &drive).await;
    let top_node = s.tree.children.iter().find(|c| c.name == "Top").unwrap();
    let folders: Vec<&TreeNode> = top_node.children.iter().filter(|c| c.kind == "folder").collect();
    assert_eq!(folders.len(), TREE_WIDTH);
    assert_eq!(folders[0].name, "d25", "the largest first");
    let other = top_node.children.iter().find(|c| c.kind == "other").unwrap();
    assert_eq!(other.files, (25 - TREE_WIDTH + 1) as i64, "what didn't fit, and the tiny tree of l1 among them");
    assert_eq!(other.bytes, (1..=5).map(|i| i * 100).sum::<i64>() + 1);
    // Down to the depth that was asked for, and no further
    fn depth(n: &TreeNode) -> usize {
        1 + n.children.iter().map(depth).max().unwrap_or(0)
    }
    assert!(depth(&s.tree) <= TREE_DEPTH + 2, "{}", depth(&s.tree));
    assert_eq!(top_node.bytes, (1..=25).map(|i| i * 100).sum::<i64>() + 1, "the totals below are all there");
}

// ───────────── Who may see what ─────────────

async fn team_space(env: &TestEnv, name: &str) -> (String, String) {
    let req = serde_json::from_value(json!({ "name": name })).unwrap();
    let Json(info) = crate::drives::create(State(env.st.clone()), env.admin().await, Json(req)).await.unwrap();
    let v = serde_json::to_value(&info).unwrap();
    (v["id"].as_str().unwrap().to_string(), v["root_id"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn restricted_folders_and_the_copies_in_them_are_left_out_for_whoever_may_not_see_them() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    let (drive, root) = team_space(&env, "Team").await;
    env.grant(&root, &amy, "manager").await;
    let open = env.folder(&admin, &root, "Open").await;
    let secret = env.folder(&admin, &root, "Secret").await;
    set(&env, "UPDATE nodes SET restricted = 1 WHERE id = ?1", &[&secret]).await;
    env.stored_file(&admin, &open, "open.bin", &[3u8; 2000]).await;
    env.stored_file(&admin, &open, "same.bin", &BIG).await;
    env.stored_file(&admin, &secret, "same-in-secret.bin", &BIG).await;
    env.stored_file(&admin, &secret, "payroll.bin", &[4u8; 9000]).await;

    let admin_sees = compute_for(&env, &admin, &drive).await;
    assert_eq!(admin_sees.files, 4);
    assert_eq!(admin_sees.duplicates.groups.len(), 1, "the owner of the space sees the restricted folder");
    let amy_sees = compute_for(&env, &amy, &drive).await;
    assert_eq!((amy_sees.files, amy_sees.live_bytes), (2, 5000));
    assert!(amy_sees.duplicates.groups.is_empty(), "a copy she can't see is not a copy she is told about");
    assert!(amy_sees.largest.iter().all(|f| f.name != "payroll.bin"));
    assert!(amy_sees.folders_by_size.iter().all(|f| f.path != "/Secret"));
    assert!(!amy_sees.tree.children.iter().any(|c| c.name == "Secret"));
    let everything = serde_json::to_string(&amy_sees).unwrap();
    assert!(!everything.contains("payroll") && !everything.contains("Secret") && !everything.contains("same-in-secret"), "{everything}");
}

#[tokio::test]
async fn who_may_look_at_what_takes_the_room() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    let ben = env.user("ben", true).await;
    let (team, root) = team_space(&env, "Team").await;
    env.grant(&root, &amy, "manager").await;
    env.grant(&root, &ben, "editor").await;
    let check = |user: &User, drive: &str| {
        let (st, user, drive) = (env.st.clone(), user.clone(), drive.to_string());
        async move { allowed(&mut st.db.acquire().await.unwrap(), &user, &drive).await.map(|d| d.id) }
    };
    // A team space: its managers and the administrators, not its other members
    assert!(check(&amy, &team).await.is_ok());
    assert!(check(&admin, &team).await.is_ok());
    assert_eq!(check(&ben, &team).await.unwrap_err().status, StatusCode::FORBIDDEN);
    let stranger = env.user("stranger", true).await;
    assert!(check(&stranger, &team).await.is_err());
    // My files: its owner, never an administrator
    let amy_files = env.drive_of(amy.root()).await;
    assert!(check(&amy, &amy_files).await.is_ok());
    assert_eq!(check(&admin, &amy_files).await.unwrap_err().status, StatusCode::FORBIDDEN);
    assert_eq!(check(&ben, &amy_files).await.unwrap_err().status, StatusCode::FORBIDDEN);
    assert_eq!(check(&admin, "no-such-space").await.unwrap_err().status, StatusCode::NOT_FOUND);
}

// ───────────── The page ─────────────

#[tokio::test]
async fn a_snapshot_is_made_by_a_task_and_kept_for_each_person() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    let (drive, root) = team_space(&env, "Team").await;
    env.grant(&root, &amy, "manager").await;
    env.stored_file(&admin, &root, "a.bin", &[1u8; 2000]).await;
    let ask = |user: &User| get(State(env.st.clone()), user.clone(), Path(drive.clone()));

    // Before the first: nothing, but the space is named
    let Json(v) = ask(&admin).await.unwrap();
    assert_eq!((v["snapshot"].clone(), v["drive"]["name"].clone(), v["drive"]["folder_space"].clone()), (Value::Null, json!("Team"), json!(false)));

    let Json(job) = refresh(State(env.st.clone()), admin.clone(), Path(drive.clone())).await.unwrap();
    assert_eq!(job.kind, "analytics");
    assert_eq!(crate::jobs::wait_for(&env.st, &job.id).await.state, "done");
    let Json(v) = ask(&admin).await.unwrap();
    assert_eq!(v["snapshot"]["files"], 1);
    assert_eq!(v["snapshot"]["live_bytes"], 2000);
    assert!(v["snapshot"]["taken_at"].as_i64().unwrap() > 0);
    // Amy has none yet: each person's is their own
    assert_eq!(ask(&amy).await.unwrap().0["snapshot"], Value::Null);

    // A new file, and a new snapshot replaces the old one
    env.stored_file(&admin, &root, "b.bin", &[2u8; 1000]).await;
    let Json(job) = refresh(State(env.st.clone()), amy.clone(), Path(drive.clone())).await.unwrap();
    crate::jobs::wait_for(&env.st, &job.id).await;
    assert_eq!(ask(&amy).await.unwrap().0["snapshot"]["files"], 2);
    assert_eq!(ask(&admin).await.unwrap().0["snapshot"]["files"], 1, "until the administrator asks again");
    let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM analytics_snapshots").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(rows, 2);
    // Someone who may not look neither reads nor starts
    let ben = env.user("ben", true).await;
    assert!(ask(&ben).await.is_err());
    assert!(refresh(State(env.st.clone()), ben, Path(drive)).await.is_err());
}

// ───────────── Folder spaces ─────────────

#[tokio::test]
async fn in_a_folder_space_copies_are_found_by_reading_the_files_that_share_a_size() {
    let env = testutil::folders_env().await;
    let admin = env.admin().await;
    let space = env.folder_space("Disk").await;
    const SAME: [u8; 4000] = [5; 4000];
    const OTHER: [u8; 4000] = [6; 4000];
    let a = env.folder(&admin, &space.root, "a").await;
    env.upload(&admin, &a, "one.bin", &SAME).await;
    env.upload(&admin, &space.root, "two.bin", &SAME).await;
    env.upload(&admin, &space.root, "three.bin", &OTHER).await;
    env.upload(&admin, &space.root, "small.txt", b"tiny").await;
    env.upload(&admin, &space.root, "small-too.txt", b"tiny").await;
    let s = compute_for(&env, &admin, &space.drive).await;
    assert!(s.folder_space);
    assert_eq!(s.duplicates.groups.len(), 1, "the same size but another content is no copy, and a few bytes are not worth a line");
    let g = &s.duplicates.groups[0];
    assert_eq!((g.copies, g.size, g.wasted), (2, 4000, 4000));
    let mut names: Vec<&str> = g.files.iter().map(|f| f.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["one.bin", "two.bin"]);
    assert!(!s.duplicates.partial);
    assert_eq!(s.files, 5);
}
