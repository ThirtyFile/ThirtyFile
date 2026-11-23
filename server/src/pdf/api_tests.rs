//! The page operations as people use them: results stored next to the original or as a new version, what each
//! person may do, quotas, and files that can't be used.

use axum::{Json, extract::State};

use super::{
    api::{self, CombineReq, PagePick, PagesReq, SplitReq, read_for_tests},
    open,
    tests::{check_structure, classic, content, labels, obj, three_pages},
};
use crate::{
    auth::User,
    error::AppResult,
    jobs::{self, Job},
    testutil::{self, TestEnv},
    tree,
};

fn pick(page: usize, rotate: i32) -> PagePick {
    PagePick { page, rotate }
}

fn pages_req(id: &str, pages: Vec<PagePick>, save: &str) -> PagesReq {
    PagesReq { id: id.into(), pages: Some(pages), ranges: None, save: save.into(), name: None, base_version: None }
}

async fn run_pages(env: &TestEnv, user: &User, req: PagesReq) -> AppResult<Job> {
    let Json(job) = api::pages(State(env.st.clone()), user.clone(), Json(req)).await?;
    Ok(jobs::wait_for(&env.st, &job.id).await)
}

async fn run_split(env: &TestEnv, user: &User, id: &str, ranges: Option<&str>) -> AppResult<Job> {
    let Json(job) = api::split(State(env.st.clone()), user.clone(), Json(SplitReq { id: id.into(), ranges: ranges.map(String::from) })).await?;
    Ok(jobs::wait_for(&env.st, &job.id).await)
}

async fn run_combine(env: &TestEnv, user: &User, ids: &[&str], name: Option<&str>) -> AppResult<Job> {
    let req = CombineReq { ids: ids.iter().map(|s| s.to_string()).collect(), name: name.map(String::from) };
    let Json(job) = api::combine_files(State(env.st.clone()), user.clone(), Json(req)).await?;
    Ok(jobs::wait_for(&env.st, &job.id).await)
}

async fn node(env: &TestEnv, id: &str) -> tree::Node {
    tree::get_node(&mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap()
}

async fn drive_used(env: &TestEnv, id: &str) -> i64 {
    let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(env.drive_of(id).await).fetch_one(&env.st.db).await.unwrap();
    used
}

/// What each page of a stored file says; a file made here is checked to be a whole PDF (the test files are not)
async fn pages_of(env: &TestEnv, id: &str) -> Vec<String> {
    let data = read_for_tests(&env.st, &node(env, id).await).await;
    if data.ends_with(b"%%EOF\n") {
        check_structure(&data);
    }
    labels(&data)
}

async fn names(env: &TestEnv, folder: &str) -> Vec<String> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM nodes WHERE parent_id = ? AND trashed_at IS NULL ORDER BY name").bind(folder).fetch_all(&env.st.db).await.unwrap();
    rows.into_iter().map(|r| r.0).collect()
}

async fn id_of(env: &TestEnv, folder: &str, name: &str) -> String {
    sqlx::query_as::<_, (String,)>("SELECT id FROM nodes WHERE parent_id = ? AND name = ? AND trashed_at IS NULL")
        .bind(folder)
        .bind(name)
        .fetch_one(&env.st.db)
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn pages_are_extracted_into_a_new_file_next_to_the_original() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let docs = env.folder(&amy, amy.root(), "Docs").await;
    let pdf = env.stored_file(&amy, &docs, "report.pdf", &three_pages()).await;
    // A range, in the order written
    let job = run_pages(&env, &amy, PagesReq { pages: None, ranges: Some("3, 1".into()), ..pages_req(&pdf, vec![], "new") }).await.unwrap();
    assert_eq!((job.state, job.name.as_deref()), ("done", Some("report (pages).pdf")), "{job:?}");
    assert_eq!(pages_of(&env, &job.node_id.unwrap()).await, ["C", "A"]);
    // A chosen name, and a name that is taken is numbered
    let named = PagesReq { name: Some("Two".into()), ..pages_req(&pdf, vec![pick(1, 0)], "new") };
    assert_eq!(run_pages(&env, &amy, named).await.unwrap().name.as_deref(), Some("Two.pdf"));
    let again = PagesReq { name: Some("Two.pdf".into()), ..pages_req(&pdf, vec![pick(1, 0)], "new") };
    assert_eq!(run_pages(&env, &amy, again).await.unwrap().name.as_deref(), Some("Two (1).pdf"));
    // The original is as it was, and the new files count in the space's usage
    assert_eq!(pages_of(&env, &pdf).await, ["A", "B", "C"]);
    assert_eq!(names(&env, &docs).await, ["Two (1).pdf", "Two.pdf", "report (pages).pdf", "report.pdf"]);
    let (sizes,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(size), 0) FROM nodes WHERE parent_id = ?").bind(&docs).fetch_one(&env.st.db).await.unwrap();
    // (the original was put there by the test helper, which doesn't count it)
    assert_eq!(drive_used(&env, &pdf).await, sizes - three_pages().len() as i64);
    // A range past the last page, or none, or nothing chosen
    for bad in [Some("1-9"), Some("0"), Some(""), None] {
        let req = PagesReq { pages: None, ranges: bad.map(String::from), ..pages_req(&pdf, vec![], "new") };
        assert_eq!(run_pages(&env, &amy, req).await.unwrap().state, "failed", "{bad:?}");
    }
    let job = run_pages(&env, &amy, pages_req(&pdf, vec![], "new")).await.unwrap();
    assert_eq!((job.state, job.error.as_deref()), ("failed", Some("Choose at least one page")));
    assert_eq!(run_pages(&env, &amy, pages_req(&pdf, vec![pick(7, 0)], "new")).await.unwrap().state, "failed");
    assert!(run_pages(&env, &amy, pages_req(&pdf, vec![pick(0, 0)], "elsewhere")).await.is_err());
    assert_eq!(names(&env, &docs).await.len(), 4);
}

#[tokio::test]
async fn organizing_can_save_a_new_version_that_can_be_restored() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let pdf = env.stored_file(&amy, amy.root(), "scan.pdf", &three_pages()).await;
    let before = node(&env, &pdf).await;
    // Reordered, one deleted, one turned
    let job = run_pages(&env, &amy, pages_req(&pdf, vec![pick(2, 90), pick(0, 0)], "version")).await.unwrap();
    assert_eq!((job.state, job.node_id.as_deref()), ("done", Some(pdf.as_str())), "{job:?}");
    assert_eq!(pages_of(&env, &pdf).await, ["C", "A"]);
    let after = node(&env, &pdf).await;
    assert_ne!(after.blob_hash, before.blob_hash);
    // Page C had a rotation of 270; 90 more is a whole turn
    let data = read_for_tests(&env.st, &after).await;
    assert_eq!(open(&data).unwrap().pages[0].dict["Rotate"], crate::pdf::object::Obj::Int(0));
    // The content it had is kept as an earlier version
    let versions: Vec<(String,)> = sqlx::query_as("SELECT id FROM node_versions WHERE node_id = ?").bind(&pdf).fetch_all(&env.st.db).await.unwrap();
    assert_eq!(versions.len(), 1);
    let restored = crate::versions::restore(State(env.st.clone()), amy.clone(), axum::extract::Path((pdf.clone(), versions[0].0.clone()))).await;
    assert!(restored.is_ok());
    assert_eq!(pages_of(&env, &pdf).await, ["A", "B", "C"]);
}

#[tokio::test]
async fn a_version_is_not_saved_over_someone_elses_newer_save() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let pdf = env.stored_file(&amy, amy.root(), "scan.pdf", &three_pages()).await;
    let opened_at = node(&env, &pdf).await.updated_at;
    // Someone else saves meanwhile
    sqlx::query("UPDATE nodes SET updated_at = ? WHERE id = ?").bind(opened_at + 5).bind(&pdf).execute(&env.st.db).await.unwrap();
    let req = PagesReq { base_version: Some(opened_at), ..pages_req(&pdf, vec![pick(0, 0)], "version") };
    let err = run_pages(&env, &amy, req).await.unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
    assert_eq!(pages_of(&env, &pdf).await, ["A", "B", "C"]);
    // With the version the page knows now, it is saved
    let req = PagesReq { base_version: Some(opened_at + 5), ..pages_req(&pdf, vec![pick(0, 0)], "version") };
    assert_eq!(run_pages(&env, &amy, req).await.unwrap().state, "done");
    assert_eq!(pages_of(&env, &pdf).await, ["A"]);
}

#[tokio::test]
async fn a_pdf_is_split_into_a_file_per_page_or_per_range() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let root = amy.root().to_string();
    let pdf = env.stored_file(&amy, &root, "book.pdf", &three_pages()).await;
    let job = run_split(&env, &amy, &pdf, None).await.unwrap();
    assert_eq!(job.state, "done", "{job:?}");
    assert_eq!(job.result.clone().unwrap()["files"], 3);
    assert_eq!(names(&env, &root).await, ["book - 1.pdf", "book - 2.pdf", "book - 3.pdf", "book.pdf"]);
    assert_eq!(pages_of(&env, &id_of(&env, &root, "book - 2.pdf").await).await, ["B"]);
    let job = run_split(&env, &amy, &pdf, Some("1-2, 3")).await.unwrap();
    assert_eq!(job.state, "done", "{job:?}");
    assert_eq!(pages_of(&env, &id_of(&env, &root, "book - 1-2.pdf").await).await, ["A", "B"]);
    // Splitting again numbers the names that are taken
    let job = run_split(&env, &amy, &pdf, Some("1-2")).await.unwrap();
    assert_eq!(job.name.as_deref(), Some("book - 1-2 (1).pdf"));
    // Bad ranges make no file
    let count = names(&env, &root).await.len();
    assert_eq!(run_split(&env, &amy, &pdf, Some("2-9")).await.unwrap().state, "failed");
    assert_eq!(names(&env, &root).await.len(), count);
}

#[tokio::test]
async fn pdfs_are_combined_in_the_order_chosen() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let root = amy.root().to_string();
    let one = env.stored_file(&amy, &root, "one.pdf", &three_pages()).await;
    let other = classic(
        &[
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 9 9] /Resources << >> >>"),
            obj(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>"),
            content(4, "X"),
        ],
        "",
    );
    let two = env.stored_file(&amy, &root, "two.pdf", &other).await;
    let job = run_combine(&env, &amy, &[&two, &one], None).await.unwrap();
    assert_eq!((job.state, job.name.as_deref()), ("done", Some("Combined.pdf")), "{job:?}");
    assert_eq!(pages_of(&env, &job.node_id.unwrap()).await, ["X", "A", "B", "C"]);
    let job = run_combine(&env, &amy, &[&one, &two], Some("Both")).await.unwrap();
    assert_eq!(pages_of(&env, &job.node_id.unwrap()).await, ["A", "B", "C", "X"]);
    // At least two, and PDFs only
    assert!(run_combine(&env, &amy, &[&one], None).await.is_err());
    let text = env.stored_file(&amy, &root, "notes.txt", b"hi").await;
    assert_eq!(run_combine(&env, &amy, &[&one, &text], None).await.unwrap_err().message, "This is not a PDF file");
}

#[tokio::test]
async fn a_person_changes_pages_only_where_they_may() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let ben = env.user("ben", true).await;
    let shared = env.folder(&amy, amy.root(), "Shared").await;
    let pdf = env.stored_file(&amy, &shared, "a.pdf", &three_pages()).await;
    // No access: not found
    let none = [
        run_pages(&env, &ben, pages_req(&pdf, vec![pick(0, 0)], "new")).await.map(|_| ()),
        run_split(&env, &ben, &pdf, None).await.map(|_| ()),
        run_combine(&env, &ben, &[&pdf, &pdf], None).await.map(|_| ()),
    ];
    for result in none {
        assert_eq!(result.unwrap_err().status, axum::http::StatusCode::NOT_FOUND);
    }
    // A viewer reads, so can't add files to the folder or change the file
    env.grant(&shared, &ben, "viewer").await;
    let viewer = [
        run_pages(&env, &ben, pages_req(&pdf, vec![pick(0, 0)], "new")).await.map(|_| ()),
        run_pages(&env, &ben, pages_req(&pdf, vec![pick(0, 0)], "version")).await.map(|_| ()),
        run_split(&env, &ben, &pdf, None).await.map(|_| ()),
        run_combine(&env, &ben, &[&pdf, &pdf], None).await.map(|_| ()),
    ];
    for result in viewer {
        assert_eq!(result.unwrap_err().status, axum::http::StatusCode::FORBIDDEN);
    }
    assert_eq!(names(&env, &shared).await, ["a.pdf"]);
    // An editor can
    env.grant(&shared, &ben, "editor").await;
    assert_eq!(run_pages(&env, &ben, pages_req(&pdf, vec![pick(0, 0)], "new")).await.unwrap().state, "done");
    assert_eq!(run_pages(&env, &ben, pages_req(&pdf, vec![pick(1, 0)], "version")).await.unwrap().state, "done");
}

#[tokio::test]
async fn the_quota_of_the_space_is_kept_to() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let root = amy.root().to_string();
    let pdf = env.stored_file(&amy, &root, "a.pdf", &three_pages()).await;
    let used = drive_used(&env, &pdf).await;
    sqlx::query("UPDATE users SET quota_bytes = ? WHERE id = ?").bind(used + 10).bind(amy.id).execute(&env.st.db).await.unwrap();
    let jobs = [
        run_pages(&env, &amy, pages_req(&pdf, vec![pick(0, 0)], "new")).await.unwrap(),
        run_split(&env, &amy, &pdf, None).await.unwrap(),
        run_combine(&env, &amy, &[&pdf, &pdf], None).await.unwrap(),
    ];
    for job in jobs {
        assert!(job.error.as_deref().unwrap_or_default().starts_with("Not enough storage space"), "{job:?}");
    }
    assert_eq!(names(&env, &root).await, ["a.pdf"]);
}

#[tokio::test]
async fn encrypted_and_damaged_files_are_refused_with_a_clear_message() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let root = amy.root().to_string();
    let encrypted = classic(&[obj(1, "<< /Type /Catalog /Pages 2 0 R >>"), obj(2, "<< /Type /Pages /Kids [] /Count 0 >>")], "/Encrypt << /Filter /Standard >>");
    let locked = env.stored_file(&amy, &root, "locked.pdf", &encrypted).await;
    let damaged = env.stored_file(&amy, &root, "broken.pdf", b"%PDF-1.4\n1 0 obj\n<< /Typ").await;
    let fine = env.stored_file(&amy, &root, "fine.pdf", &three_pages()).await;
    for id in [&locked, &damaged] {
        let job = run_pages(&env, &amy, pages_req(id, vec![pick(0, 0)], "new")).await.unwrap();
        assert_eq!(job.state, "failed");
        let error = job.error.as_deref().unwrap();
        assert!(error.contains("encrypted") || error.contains("damaged"), "{job:?}");
        assert_eq!(run_split(&env, &amy, id, None).await.unwrap().state, "failed");
    }
    let job = run_combine(&env, &amy, &[&fine, &locked], None).await.unwrap();
    assert_eq!(job.error.as_deref(), Some("\"locked.pdf\" can't be combined: This PDF is encrypted. Remove its password first, then try again"));
    assert_eq!(names(&env, &root).await, ["broken.pdf", "fine.pdf", "locked.pdf"]);
}
