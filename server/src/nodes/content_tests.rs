//! Searching inside files, as people see it: what is returned and with what snippet, and what is never shown

use super::*;
use crate::{auth::Admin, contentsearch, testutil};
use axum::extract::Query;

async fn turn_on(env: &testutil::TestEnv) {
    let admin = env.admin().await;
    let config = contentsearch::Config { enabled: true, ..Default::default() };
    let _ = contentsearch::save(State(env.st.clone()), Admin(admin), Json(config)).await.unwrap();
}

/// What a search returns for `user`: the names found, and the snippets by name
async fn look(env: &testutil::TestEnv, user: &crate::auth::User, term: &str, content: Option<bool>) -> (Vec<String>, Vec<(String, Vec<String>)>) {
    let q =
        SearchQuery { q: term.into(), within: None, kind: None, ext: None, from: None, to: None, min_size: None, max_size: None, owner: None, tags: None, content };
    let Json(r) = search(State(env.st.clone()), user.clone(), Query(q)).await.unwrap();
    let mut names: Vec<String> = r.items.iter().map(|l| l.node.name.clone()).collect();
    names.sort();
    let mut snippets: Vec<(String, Vec<String>)> = r.items.iter().filter_map(|l| r.snippets.get(&l.node.id).map(|s| (l.node.name.clone(), s.clone()))).collect();
    snippets.sort();
    (names, snippets)
}

#[tokio::test]
async fn a_search_looks_inside_files_only_when_asked_and_shows_where_it_found_the_words() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    turn_on(&env).await;
    env.stored_file(&amy, amy.root(), "Minutes.txt", b"We agreed that the cafeteria renovation starts in May.").await;
    env.stored_file(&amy, amy.root(), "renovation-budget.csv", b"item,cost\npaint,100").await;
    env.stored_file(&amy, amy.root(), "photo.jpg", b"renovation (not text)").await;
    contentsearch::read_everything(&env.st).await;

    // By name only, as the address bar searches
    let (names, snippets) = look(&env, &amy, "renovation", None).await;
    assert_eq!(names, ["renovation-budget.csv"]);
    assert!(snippets.is_empty());
    // Inside files too: a file is found by its name or by what it holds, and shows a snippet only for the latter
    let (names, snippets) = look(&env, &amy, "renovation", Some(true)).await;
    assert_eq!(names, ["Minutes.txt", "renovation-budget.csv"]);
    assert_eq!(snippets.len(), 1);
    let (name, pieces) = &snippets[0];
    assert_eq!(name, "Minutes.txt");
    assert_eq!(pieces.iter().skip(1).step_by(2).collect::<Vec<_>>(), ["renovation"], "{pieces:?}");
    assert!(pieces[0].contains("cafeteria") && pieces[2].contains("May"), "{pieces:?}");
    // Several words: all of them, wherever they are in the file
    assert_eq!(look(&env, &amy, "starts cafeteria", Some(true)).await.0, ["Minutes.txt"]);
    assert!(look(&env, &amy, "starts winter", Some(true)).await.0.is_empty());
    // Two characters are too few for the index: names only
    assert!(look(&env, &amy, "ma", Some(true)).await.1.is_empty());
}

#[tokio::test]
async fn searching_inside_files_is_ignored_while_it_is_off_and_after_it_is_turned_off() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.stored_file(&amy, amy.root(), "a.txt", b"hidden treasure inside").await;
    assert!(look(&env, &amy, "treasure", Some(true)).await.0.is_empty(), "off: nothing was read");
    turn_on(&env).await;
    contentsearch::read_everything(&env.st).await;
    assert_eq!(look(&env, &amy, "treasure", Some(true)).await.0, ["a.txt"]);
    let admin = env.admin().await;
    let _ = contentsearch::save(State(env.st.clone()), Admin(admin), Json(contentsearch::Config::default())).await.unwrap();
    let (names, snippets) = look(&env, &amy, "treasure", Some(true)).await;
    assert!(names.is_empty() && snippets.is_empty(), "turned off: the text is gone");
}

#[tokio::test]
async fn what_a_person_cannot_open_is_never_found_inside_nor_shown_in_a_snippet() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let bob = env.user("bob", true).await;
    turn_on(&env).await;
    // Amy's own file, a folder shared with Bob with a restricted folder inside, and a file in the trash
    env.stored_file(&amy, amy.root(), "private.txt", b"the launch codename is falcon").await;
    let shared = env.folder(&amy, amy.root(), "Shared").await;
    env.stored_file(&amy, &shared, "open.txt", b"public falcon plans").await;
    let hr = env.folder(&amy, &shared, "HR").await;
    env.stored_file(&amy, &hr, "salaries.txt", b"falcon team salaries").await;
    sqlx::query("UPDATE nodes SET restricted = 1 WHERE id = ?").bind(&hr).execute(&env.st.db).await.unwrap();
    let old = env.stored_file(&amy, &shared, "old.txt", b"falcon history").await;
    sqlx::query("UPDATE nodes SET trashed_at = unixepoch() WHERE id = ?").bind(&old).execute(&env.st.db).await.unwrap();
    env.grant(&shared, &bob, "editor").await;
    contentsearch::read_everything(&env.st).await;

    // Amy sees all that is hers that is not in the trash
    let (names, snippets) = look(&env, &amy, "falcon", Some(true)).await;
    assert_eq!(names, ["open.txt", "private.txt", "salaries.txt"]);
    assert_eq!(snippets.len(), 3);
    // Bob sees the shared folder's open file and nothing of the restricted folder, Amy's other files or the trash,
    // neither by name nor by snippet
    let (names, snippets) = look(&env, &bob, "falcon", Some(true)).await;
    assert_eq!(names, ["open.txt"]);
    assert_eq!(snippets.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(), ["open.txt"]);
    let everything = format!("{snippets:?}");
    assert!(!everything.contains("salaries") && !everything.contains("codename") && !everything.contains("history"), "{everything}");
    // Someone with nothing shared finds nothing
    let cy = env.user("cy", true).await;
    let (names, snippets) = look(&env, &cy, "falcon", Some(true)).await;
    assert!(names.is_empty() && snippets.is_empty());
    // Coming back from the trash makes it searchable again
    sqlx::query("UPDATE nodes SET trashed_at = NULL WHERE id = ?").bind(&old).execute(&env.st.db).await.unwrap();
    contentsearch::read_everything(&env.st).await;
    assert_eq!(look(&env, &bob, "history", Some(true)).await.0, ["old.txt"]);
}

#[tokio::test]
async fn a_smart_folder_can_look_inside_files_and_goes_back_to_names_when_that_is_turned_off() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    turn_on(&env).await;
    env.stored_file(&amy, amy.root(), "notes.txt", b"remember the milkshake").await;
    env.stored_file(&amy, amy.root(), "milkshake-recipe.txt", b"blend").await;
    contentsearch::read_everything(&env.st).await;
    let req = serde_json::from_value(serde_json::json!({ "name": "Shakes", "query": { "name": "milkshake", "content": true } })).unwrap();
    let Json(folder) = create_smart_folder(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
    let id = serde_json::to_value(&folder).unwrap()["id"].as_i64().unwrap();
    let list = || async {
        let Json(l) = smart_items(State(env.st.clone()), amy.clone(), Path(id), Query(ListQuery::default())).await.unwrap();
        let mut names: Vec<String> = match l {
            Listing::All(items) => items.into_iter().map(|i| i.node.name).collect(),
            Listing::Page { items, .. } => items.into_iter().map(|i| i.node.name).collect(),
        };
        names.sort();
        names
    };
    assert_eq!(list().await, ["milkshake-recipe.txt", "notes.txt"]);
    let admin = env.admin().await;
    let _ = contentsearch::save(State(env.st.clone()), Admin(admin), Json(contentsearch::Config::default())).await.unwrap();
    assert_eq!(list().await, ["milkshake-recipe.txt"]);
}
