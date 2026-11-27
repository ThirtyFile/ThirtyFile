//! Renaming several items at once: all of them or none, names swapped among them, and folder spaces on disk.

use axum::{Json, extract::State, http::StatusCode};
use serde_json::json;

use crate::{
    auth::User,
    error::AppResult,
    fsops::testing::{self, Stop},
    testutil::{self, FolderSpace, TestEnv, write_old},
    tree::{self, Node},
};

use super::rename_many;

async fn rename(env: &TestEnv, user: &User, items: &[(&str, &str)]) -> AppResult<Vec<Node>> {
    let items: Vec<_> = items.iter().map(|(id, name)| json!({ "id": id, "name": name })).collect();
    let req = Json(serde_json::from_value(json!({ "items": items })).unwrap());
    rename_many(State(env.st.clone()), user.clone(), req).await.map(|Json(nodes)| nodes)
}

async fn node(env: &TestEnv, id: &str) -> Node {
    tree::get_node(&mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap()
}

async fn names(env: &TestEnv, ids: &[&String]) -> Vec<String> {
    let mut out = Vec::new();
    for id in ids {
        out.push(node(env, id).await.name);
    }
    out
}

async fn logged(env: &TestEnv) -> i64 {
    sqlx::query_as::<_, (i64,)>("SELECT count(*) FROM activity WHERE action = 'rename'").fetch_one(&env.st.db).await.unwrap().0
}

#[tokio::test]
async fn several_items_are_renamed_at_once() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let a = env.file(&amy, amy.root(), "a.txt").await;
    let b = env.file(&amy, amy.root(), "b.txt").await;
    let docs = env.folder(&amy, amy.root(), "Docs").await;
    let inside = env.file(&amy, &docs, "c.txt").await;

    // Items of different folders, a folder, and one whose name doesn't change
    let renamed = rename(&env, &amy, &[(&a, "1.md"), (&b, "b.txt"), (&docs, "Papers")]).await.unwrap();
    assert_eq!(renamed.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["1.md", "Papers"]);
    assert_eq!(names(&env, &[&a, &b, &docs, &inside]).await, ["1.md", "b.txt", "Papers", "c.txt"]);
    assert_eq!(node(&env, &a).await.mime, crate::util::guess_mime("1.md"));
    assert_eq!(logged(&env).await, 2);
}

#[tokio::test]
async fn names_can_be_swapped_passed_along_and_changed_in_letter_case() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let a = env.file(&amy, amy.root(), "a.txt").await;
    let b = env.file(&amy, amy.root(), "b.txt").await;
    let c = env.file(&amy, amy.root(), "c.txt").await;

    rename(&env, &amy, &[(&a, "b.txt"), (&b, "a.txt")]).await.unwrap();
    assert_eq!(names(&env, &[&a, &b]).await, ["b.txt", "a.txt"]);

    // b ← a ← c ← d: each takes the name the next one leaves
    rename(&env, &amy, &[(&b, "c.txt"), (&c, "d.txt")]).await.unwrap();
    assert_eq!(names(&env, &[&b, &c]).await, ["c.txt", "d.txt"]);

    // Only the letter case, also onto a name that differs only in case and is given up
    rename(&env, &amy, &[(&a, "B.TXT"), (&b, "ÉTÉ.txt"), (&c, "b2.txt")]).await.unwrap();
    rename(&env, &amy, &[(&b, "été.txt")]).await.unwrap();
    assert_eq!(names(&env, &[&a, &b, &c]).await, ["B.TXT", "été.txt", "b2.txt"]);
}

#[tokio::test]
async fn nothing_is_renamed_when_one_item_cant_be() {
    let env = testutil::env().await;
    let owner = env.user("owner", true).await;
    let editor = env.user("editor", true).await;
    let mine = env.folder(&owner, owner.root(), "Edit").await;
    let seen = env.folder(&owner, owner.root(), "See").await;
    env.grant(&mine, &editor, "editor").await;
    env.grant(&seen, &editor, "viewer").await;
    let a = env.file(&owner, &mine, "a.txt").await;
    let b = env.file(&owner, &mine, "b.txt").await;
    let kept = env.file(&owner, &mine, "kept.txt").await;
    let theirs = env.file(&owner, &seen, "theirs.txt").await;
    let unchanged = async || {
        assert_eq!(names(&env, &[&a, &b, &kept, &theirs, &mine]).await, ["a.txt", "b.txt", "kept.txt", "theirs.txt", "Edit"]);
        assert_eq!(logged(&env).await, 0);
    };

    // A name an item keeps, one given twice (also in other letter case), and one no item can have
    for second in ["KEPT.txt", "x.txt", "X.TXT", "no/slash"] {
        let error = rename(&env, &editor, &[(&a, "x.txt"), (&b, second)]).await.unwrap_err();
        assert!(matches!(error.status, StatusCode::CONFLICT | StatusCode::BAD_REQUEST), "{second}: {}", error.message);
        unchanged().await;
    }
    let error = rename(&env, &editor, &[(&a, "x.txt"), (&theirs, "y.txt")]).await.unwrap_err();
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    unchanged().await;
    let error = rename(&env, &owner, &[(&mine, "Edited"), (&a, "x.txt")]).await.unwrap_err();
    assert_eq!(error.message, "Rename a folder or the items in it, not both at once");
    unchanged().await;
    let error = rename(&env, &owner, &[(&a, "x.txt"), (&a, "y.txt")]).await.unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    let error = rename(&env, &owner, &[(owner.root(), "Home")]).await.unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(rename(&env, &owner, &[]).await.is_err());
    unchanged().await;
}

/// A folder space with two files and two folders holding a file each
async fn space(env: &TestEnv) -> (FolderSpace, [String; 4]) {
    let space = env.folder_space("Shared").await;
    for (path, content) in [("a.txt", "a"), ("b.txt", "bb"), ("One/in.txt", "one"), ("Two/in.txt", "second")] {
        write_old(&space.dir.join(path), content.as_bytes());
    }
    crate::folders::scan(&env.st, &space.drive).await.unwrap();
    let mut ids: [String; 4] = Default::default();
    for (id, path) in ids.iter_mut().zip(["a.txt", "b.txt", "One", "Two"]) {
        *id = env.node_at(&space.drive, path).await.unwrap().0;
    }
    (space, ids)
}

/// The items and what the files hold are where they were before anything was renamed
async fn as_made(env: &TestEnv, space: &FolderSpace, [a, b, one, two]: &[String; 4]) {
    for (path, content, id) in [("a.txt", "a", Some(a)), ("b.txt", "bb", Some(b)), ("One/in.txt", "one", None), ("Two/in.txt", "second", None)] {
        assert_eq!(std::fs::read_to_string(space.dir.join(path)).unwrap(), content, "{path}");
        let found = env.node_at(&space.drive, path).await.unwrap_or_else(|| panic!("{path} isn't indexed"));
        assert!(id.is_none_or(|id| *id == found.0), "{path} is another item");
    }
    assert_eq!(env.node_at(&space.drive, "One").await.unwrap().0, *one);
    assert_eq!(env.node_at(&space.drive, "Two").await.unwrap().0, *two);
    assert_eq!(left_over(space), 0);
}

/// What changes left in the space's folder: journal entries, and items under names scans ignore
fn left_over(space: &FolderSpace) -> usize {
    let entries = std::fs::read_dir(space.dir.join(crate::fsops::JOURNAL_DIR)).map_or(0, |d| d.count());
    entries + std::fs::read_dir(&space.dir).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with(crate::fsops::RENAME_PREFIX)).count()
}

/// Waits until what the change left to finish is finished (the space's lock is free), and the journal entries removed
async fn settled(env: &TestEnv, space: &FolderSpace) {
    crate::tree::changes::settled(&env.st).await;
    drop(crate::fsops::lock_space(&env.st, &space.drive).await);
    for _ in 0..500 {
        if left_over(space) == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the change left something behind");
}

#[tokio::test]
async fn items_of_a_folder_space_swap_names_on_disk() {
    let _batches = crate::tree::changes::small_batches(1);
    let env = testutil::env().await;
    let admin = env.admin().await;
    let (space, [a, b, one, two]) = space(&env).await;

    rename(&env, &admin, &[(&a, "b.txt"), (&b, "a.txt"), (&one, "Two"), (&two, "One")]).await.unwrap();
    settled(&env, &space).await;
    for (path, content) in [("b.txt", "a"), ("a.txt", "bb"), ("Two/in.txt", "one"), ("One/in.txt", "second")] {
        assert_eq!(std::fs::read_to_string(space.dir.join(path)).unwrap(), content, "{path}");
    }
    assert_eq!(env.node_at(&space.drive, "b.txt").await.unwrap().0, a);
    assert_eq!(env.node_at(&space.drive, "Two").await.unwrap().0, one);
    // What is in the folders moved with them
    let moved: Vec<(String, String)> = sqlx::query_as("SELECT parent_id, fs_path FROM nodes WHERE drive_id = ? AND name = 'in.txt' ORDER BY fs_path")
        .bind(&space.drive)
        .fetch_all(&env.st.db)
        .await
        .unwrap();
    assert_eq!(moved, [(two.clone(), "One/in.txt".to_string()), (one.clone(), "Two/in.txt".to_string())]);
    // A scan finds the disk as the index has it
    let report = crate::folders::scan(&env.st, &space.drive).await.unwrap();
    assert_eq!(format!("{} {} {}", report.added, report.moved, report.removed), "0 0 0");

    // Only the letter case, and a name passed along
    rename(&env, &admin, &[(&a, "B.TXT"), (&b, "c.txt")]).await.unwrap();
    settled(&env, &space).await;
    assert_eq!(env.node_at(&space.drive, "B.TXT").await.unwrap().0, a);
    assert_eq!(std::fs::read_to_string(space.dir.join("c.txt")).unwrap(), "bb");
}

#[tokio::test]
async fn a_rename_that_fails_on_disk_puts_the_others_back() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let (space, ids) = space(&env).await;
    let [a, b, one, two] = &ids;
    // Made on the server since the last check: the index doesn't know it
    std::fs::write(space.dir.join("new.txt"), "in the way").unwrap();

    let error = rename(&env, &admin, &[(one, "Two"), (two, "One"), (a, "b.txt"), (b, "new.txt")]).await.unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT, "{}", error.message);
    assert_eq!(std::fs::read_to_string(space.dir.join("new.txt")).unwrap(), "in the way");
    // (journal entries go as their renames are put back)
    as_made(&env, &space, &ids).await;
    assert_eq!(logged(&env).await, 0);
}

#[tokio::test]
async fn renames_of_a_change_that_stopped_are_undone_by_the_next_check() {
    // Two folders and two files swapped: four renames aside, then four to the new names
    for stop in 0..8 {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let (space, ids) = space(&env).await;
        let [a, b, one, two] = &ids;
        let _stop = testing::stop_at(&space.drive, Stop::Renamed(stop));
        assert!(rename(&env, &admin, &[(a, "b.txt"), (b, "a.txt"), (one, "Two"), (two, "One")]).await.is_err());
        assert!(left_over(&space) > 0, "stop {stop}");

        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        as_made(&env, &space, &ids).await;
        assert_eq!(names(&env, &[a, b, one, two]).await, ["a.txt", "b.txt", "One", "Two"], "stop {stop}");
    }
}

#[tokio::test]
async fn an_item_that_cant_go_back_keeps_its_id() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let (space, ids) = space(&env).await;
    let [a, b, ..] = &ids;
    // Stops with both files aside and the first under its new name
    let _stop = testing::stop_at(&space.drive, Stop::Renamed(2));
    assert!(rename(&env, &admin, &[(a, "b.txt"), (b, "a.txt")]).await.is_err());
    // Someone makes a file where the second one was
    write_old(&space.dir.join("a.txt"), b"another");

    crate::folders::scan(&env.st, &space.drive).await.unwrap();
    assert_eq!(left_over(&space), 0);
    // The first can't take its name back either way round: what has a.txt stays, and both files are still the items
    let (first, second) = (node(&env, a).await, node(&env, b).await);
    assert_eq!(std::fs::read_to_string(space.dir.join(first.fs_path.unwrap())).unwrap(), "a");
    assert_eq!(std::fs::read_to_string(space.dir.join(second.fs_path.unwrap())).unwrap(), "bb");
    assert!(first.trashed_at.is_none() && second.trashed_at.is_none());
}
