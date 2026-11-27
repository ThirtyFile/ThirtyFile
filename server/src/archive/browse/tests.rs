use axum::http::StatusCode;

use super::*;
use crate::{
    archive::{ExtractReq, extract},
    jobs::{self, Job},
    testutil::{self, TestEnv},
    zip::ZipWriter,
};

/// A ZIP made with the writer, names written as given: (path, content); a path ending in "/" is a folder
async fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::deflating(Vec::new()).raw_names();
    for (path, data) in entries {
        if path.ends_with('/') {
            zip.add_dir(path, 1_700_000_000).await.unwrap();
        } else {
            zip.add_file(path, *data, data.len() as u64, 1_700_000_000).await.unwrap();
        }
    }
    zip.finish().await.unwrap()
}

/// Changes the field at `at` of the central directory's record of the entry `name`
fn patch(zip: &mut [u8], name: &str, at: usize, value: &[u8]) {
    let record = (0..zip.len() - 46)
        .find(|&i| zip[i..i + 4] == 0x0201_4b50u32.to_le_bytes() && zip[i + 46..].starts_with(name.as_bytes()))
        .expect("the entry is in the central directory");
    zip[record + at..record + at + value.len()].copy_from_slice(value);
}

/// The same archive with its end written the ZIP64 way
fn as_zip64(mut zip: Vec<u8>) -> Vec<u8> {
    let end = zip.split_off(zip.len() - 22);
    let count = u64::from(u16::from_le_bytes([end[10], end[11]]));
    let u32_at = |i: usize| u64::from(u32::from_le_bytes(end[i..i + 4].try_into().unwrap()));
    let (size, offset) = (u32_at(12), u32_at(16));
    let record = zip.len() as u64;
    zip.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
    zip.extend_from_slice(&44u64.to_le_bytes());
    zip.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    for n in [count, count, size, offset] {
        zip.extend_from_slice(&n.to_le_bytes());
    }
    zip.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
    zip.extend_from_slice(&0u32.to_le_bytes());
    zip.extend_from_slice(&record.to_le_bytes());
    zip.extend_from_slice(&1u32.to_le_bytes());
    let mut end = end;
    end[8..20].fill(0xFF);
    zip.extend(end);
    zip
}

async fn list(env: &TestEnv, user: &User, id: &str) -> AppResult<Listing> {
    entries(State(env.st.clone()), user.clone(), Path(id.into())).await.map(|Json(l)| l)
}

async fn download(env: &TestEnv, user: &User, id: &str, index: usize, version: &str) -> AppResult<Response> {
    entry(State(env.st.clone()), user.clone(), Path((id.into(), index)), Query(EntryQuery { v: version.into() }), HeaderMap::new()).await
}

async fn content_of(env: &TestEnv, user: &User, id: &str, listing: &Listing, path: &str) -> Vec<u8> {
    let found = listing.entries.iter().find(|e| e.path == path).unwrap_or_else(|| panic!("{path} isn't listed"));
    let res = download(env, user, id, found.index, &listing.version).await.unwrap();
    assert_eq!(res.headers()[header::CONTENT_LENGTH], found.size.to_string().as_str());
    axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()
}

fn shown(listing: &Listing) -> Vec<(&str, bool, u64, Option<&str>)> {
    listing.entries.iter().map(|e| (e.path.as_str(), e.dir, e.size, e.locked)).collect()
}

/// Text larger than one piece read or decompressed at a time, which deflate doesn't shrink to almost nothing
fn long_text() -> Vec<u8> {
    let mut n = 1u64;
    (0..200_000)
        .map(|_| {
            n = n.wrapping_mul(6364136223846793005).wrapping_add(1);
            b'a' + (n >> 59) as u8
        })
        .collect()
}

#[tokio::test]
async fn a_zip_file_is_listed_and_a_file_in_it_downloaded() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let long = long_text();
    let zip = zip_of(&[("Docs/", b""), ("Docs/報告.txt", &long), ("Docs/empty", b""), ("a.html", b"<script>")]).await;
    for (name, zip) in [("plain.zip", zip.clone()), ("large.zip", as_zip64(zip))] {
        let id = env.stored_file(&amy, amy.root(), name, &zip).await;
        let listing = list(&env, &amy, &id).await.unwrap();
        assert_eq!(
            shown(&listing),
            [("Docs", true, 0, None), ("Docs/報告.txt", false, long.len() as u64, None), ("Docs/empty", false, 0, None), ("a.html", false, 8, None)]
        );
        assert_eq!(listing.hidden, 0);
        // As written: the writer gave it that clock time
        assert_eq!(listing.entries[1].modified, Some(1_700_000_000));
        assert!(content_of(&env, &amy, &id, &listing, "Docs/報告.txt").await == long, "{name}");
        assert_eq!(content_of(&env, &amy, &id, &listing, "Docs/empty").await, b"");

        // Saved under its own name, never run or shown as a page
        let res = download(&env, &amy, &id, 3, &listing.version).await.unwrap();
        let h = res.headers();
        assert_eq!(h[header::CONTENT_TYPE], "application/octet-stream");
        assert!(h[header::CONTENT_DISPOSITION].to_str().unwrap().starts_with("attachment;"));
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        let mut as_script = HeaderMap::new();
        as_script.insert("sec-fetch-dest", HeaderValue::from_static("script"));
        let refused = entry(State(env.st.clone()), amy.clone(), Path((id.clone(), 3)), Query(EntryQuery { v: listing.version.clone() }), as_script).await;
        assert_eq!(refused.unwrap_err().status, StatusCode::FORBIDDEN);

        // A folder isn't a file to download, nor is a number past the end
        for index in [0, 4] {
            assert_eq!(download(&env, &amy, &id, index, &listing.version).await.unwrap_err().status, StatusCode::NOT_FOUND);
        }
        assert_eq!(download(&env, &amy, &id, 1, "another").await.unwrap_err().status, StatusCode::CONFLICT);
    }
}

#[tokio::test]
async fn a_zip_file_in_a_folder_on_the_server_is_read_in_place() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let space = env.folder_space("Shared").await;
    testutil::write_old(&space.dir.join("a.zip"), &zip_of(&[("in.txt", b"inside")]).await);
    crate::folders::scan(&env.st, &space.drive).await.unwrap();
    let (id, _) = env.node_at(&space.drive, "a.zip").await.unwrap();
    let listing = list(&env, &admin, &id).await.unwrap();
    assert_eq!(content_of(&env, &admin, &id, &listing, "in.txt").await, b"inside");

    // Changed on the server since: the list's numbers may mean other entries now
    std::fs::write(space.dir.join("a.zip"), zip_of(&[("other.txt", b"something else")]).await).unwrap();
    assert_eq!(download(&env, &admin, &id, 0, &listing.version).await.unwrap_err().status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn only_whoever_may_download_a_zip_file_looks_into_it() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let ben = env.user("ben", true).await;
    let id = env.stored_file(&amy, amy.root(), "a.zip", &zip_of(&[("a.txt", b"alpha")]).await).await;
    let listing = list(&env, &amy, &id).await.unwrap();
    assert!(list(&env, &ben, &id).await.is_err());
    assert!(download(&env, &ben, &id, 0, &listing.version).await.is_err());
    let text = env.stored_file(&amy, amy.root(), "a.txt", b"text").await;
    assert_eq!(list(&env, &amy, &text).await.err().unwrap().status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn entries_that_cant_be_had_are_marked_or_left_out() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let mut zip =
        zip_of(&[("locked.txt", b"secret"), ("odd.txt", b"other"), ("huge.bin", b"tiny"), ("../out.txt", b"x"), ("/root.txt", b"x"), ("bad.txt", b"content")]).await;
    patch(&mut zip, "locked.txt", 8, &0x0809u16.to_le_bytes());
    patch(&mut zip, "odd.txt", 10, &12u16.to_le_bytes());
    patch(&mut zip, "huge.bin", 24, &(100u32 << 20).to_le_bytes());
    patch(&mut zip, "bad.txt", 16, &0u32.to_le_bytes());
    let id = env.stored_file(&amy, amy.root(), "a.zip", &zip).await;
    let listing = list(&env, &amy, &id).await.unwrap();
    assert_eq!(
        shown(&listing),
        [
            ("locked.txt", false, 6, Some("password")),
            ("odd.txt", false, 5, Some("method")),
            ("huge.bin", false, 100 << 20, Some("size")),
            ("bad.txt", false, 7, None)
        ]
    );
    assert_eq!(listing.hidden, 2);
    for index in [0, 1, 2] {
        assert_eq!(download(&env, &amy, &id, index, &listing.version).await.unwrap_err().status, StatusCode::BAD_REQUEST);
    }
    // The wrong CRC shows once the content is out: the download fails rather than end as if all were well
    let res = download(&env, &amy, &id, 5, &listing.version).await.unwrap();
    assert!(axum::body::to_bytes(res.into_body(), usize::MAX).await.is_err());

    // One that states less than comes out of it
    let mut short = zip_of(&[("short.txt", &long_text())]).await;
    patch(&mut short, "short.txt", 24, &10u32.to_le_bytes());
    let id = env.stored_file(&amy, amy.root(), "short.zip", &short).await;
    let listing = list(&env, &amy, &id).await.unwrap();
    let res = download(&env, &amy, &id, 0, &listing.version).await.unwrap();
    assert!(axum::body::to_bytes(res.into_body(), usize::MAX).await.is_err());
}

#[tokio::test]
async fn a_damaged_archive_or_one_with_too_many_entries_isnt_listed() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let id = env.stored_file(&amy, amy.root(), "broken.zip", b"not a zip at all").await;
    let error = list(&env, &amy, &id).await.err().unwrap();
    assert_eq!((error.status, error.message.as_str()), (StatusCode::BAD_REQUEST, "This ZIP file is damaged and can't be opened"));

    let names: Vec<String> = (0..=MAX_EXTRACT_ENTRIES).map(|i| format!("d{i}/")).collect();
    let many: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), b"".as_slice())).collect();
    let id = env.stored_file(&amy, amy.root(), "many.zip", &zip_of(&many).await).await;
    let error = list(&env, &amy, &id).await.err().unwrap();
    assert!(error.message.ends_with("more than can be shown"), "{}", error.message);
}

async fn extract_paths(env: &TestEnv, user: &User, id: &str, paths: &[&str], version: &str) -> AppResult<Job> {
    let req = ExtractReq { id: id.into(), paths: Some(paths.iter().map(|p| p.to_string()).collect()), version: Some(version.into()) };
    let Json(job) = extract(State(env.st.clone()), user.clone(), Json(req)).await?;
    Ok(jobs::wait_for(&env.st, &job.id).await)
}

/// The paths of what is in a folder, folders ending in "/"
async fn tree_of(env: &TestEnv, folder: &str) -> Vec<String> {
    sqlx::query_as::<_, (String,)>(
        "WITH RECURSIVE sub(id, path, kind) AS (
           SELECT id, name, kind FROM nodes WHERE parent_id = ?1
           UNION ALL SELECT c.id, sub.path || '/' || c.name, c.kind FROM nodes c JOIN sub ON c.parent_id = sub.id
         )
         SELECT path || CASE kind WHEN 'folder' THEN '/' ELSE '' END FROM sub ORDER BY path",
    )
    .bind(folder)
    .fetch_all(&env.st.db)
    .await
    .unwrap()
    .into_iter()
    .map(|(p,)| p)
    .collect()
}

#[tokio::test]
async fn a_selection_of_a_zip_file_is_extracted_into_a_new_folder() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let ben = env.user("ben", true).await;
    let docs = env.folder(&amy, amy.root(), "Docs").await;
    // Entries that can't be extracted stand in nobody's way unless they are chosen
    let zip = zip_of(&[("a/b/one.txt", b"1"), ("a/b/sub/two.txt", b"2"), ("a/b/other.txt", b"3"), ("a/c.txt", b"4"), ("ab.txt", b"5"), ("../out.txt", b"x")]).await;
    let id = env.stored_file(&amy, &docs, "pack.zip", &zip).await;
    let listing = list(&env, &amy, &id).await.unwrap();
    let v = listing.version.as_str();

    // A file and a folder implied by paths alone, chosen where they are: the new folder stands for the one around them
    let job = extract_paths(&env, &amy, &id, &["a/b/one.txt", "a/b/sub"], v).await.unwrap();
    assert_eq!((job.state, job.name.as_deref(), job.error), ("done", Some("pack"), None));
    assert_eq!(tree_of(&env, &job.node_id.unwrap()).await, ["one.txt", "sub/", "sub/two.txt"]);
    // From different folders: kept apart as they are under the folder both are in
    let job = extract_paths(&env, &amy, &id, &["a/b/one.txt", "a/c.txt"], v).await.unwrap();
    assert_eq!(tree_of(&env, &job.node_id.unwrap()).await, ["b/", "b/one.txt", "c.txt"]);
    // A folder at the top, which "ab.txt" isn't part of
    let job = extract_paths(&env, &amy, &id, &["a"], v).await.unwrap();
    assert_eq!(tree_of(&env, &job.node_id.unwrap()).await, ["a/", "a/b/", "a/b/one.txt", "a/b/other.txt", "a/b/sub/", "a/b/sub/two.txt", "a/c.txt"]);

    let job = extract_paths(&env, &amy, &id, &["nowhere", "../out.txt"], v).await.unwrap();
    assert_eq!((job.state, job.error.as_deref()), ("failed", Some("The selected items aren't in this ZIP file")));
    assert_eq!(extract_paths(&env, &amy, &id, &["a"], "another").await.unwrap_err().status, StatusCode::CONFLICT);
    assert_eq!(extract_paths(&env, &amy, &id, &[], v).await.unwrap_err().status, StatusCode::BAD_REQUEST);
    assert!(extract_paths(&env, &ben, &id, &["a"], v).await.is_err());
    // Seeing the folder isn't enough to add to it
    env.grant(&docs, &ben, "viewer").await;
    assert_eq!(extract_paths(&env, &ben, &id, &["a"], v).await.unwrap_err().status, StatusCode::FORBIDDEN);
}
