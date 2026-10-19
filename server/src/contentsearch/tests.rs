//! Search inside files: what is read from each kind of file, the job that reads them, and the limits

use axum::{Json, extract::State};

use super::{
    Config, extract,
    extract::Kind,
    indexer::{self, Outcome},
    zipread::Archive,
};
use crate::{auth::Admin, testutil};

/// A ZIP of the given files, deflated, as an Office document is
async fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
    let mut zip = crate::zip::ZipWriter::deflating(Vec::new());
    for (name, content) in files {
        zip.add_file(name, content.as_bytes(), content.len() as u64, 1_700_000_000).await.unwrap();
    }
    zip.finish().await.unwrap()
}

fn text_of(kind: Kind, data: &[u8]) -> String {
    extract::read(kind, data).unwrap()
}

#[test]
fn the_kinds_of_file_that_are_read_are_told_by_name_and_type() {
    let k = |n: &str, m: &str| extract::kind_of(n, m);
    assert_eq!(k("notes.TXT", ""), Some(Kind::Text));
    assert_eq!(k("main.rs", "application/octet-stream"), Some(Kind::Text));
    assert_eq!(k("README", ""), Some(Kind::Text));
    assert_eq!(k("page.html", "text/html"), Some(Kind::Markup));
    assert_eq!(k("Report.docx", ""), Some(Kind::Docx));
    assert_eq!(k("Budget.xlsx", ""), Some(Kind::Xlsx));
    assert_eq!(k("Deck.pptx", ""), Some(Kind::Pptx));
    assert_eq!(k("letter.odt", ""), Some(Kind::Odf));
    assert_eq!(k("no-extension", "text/plain"), Some(Kind::Text));
    // Not read: pictures, archives, PDFs (for now), what has no type
    for name in ["photo.jpg", "backup.zip", "paper.pdf", "movie.mp4", "data.bin", "no-extension"] {
        assert_eq!(k(name, ""), None, "{name}");
    }
}

#[test]
fn plain_text_is_squashed_decoded_and_refused_when_it_is_not_text() {
    assert_eq!(text_of(Kind::Text, b"  Quarterly\r\n\r\n   report\t2026  "), "Quarterly report 2026");
    // A byte order mark is not part of the text; UTF-16 is read
    assert_eq!(text_of(Kind::Text, b"\xEF\xBB\xBFhello world"), "hello world");
    let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("季度報告 hello".encode_utf16().flat_map(u16::to_le_bytes)).collect();
    assert_eq!(text_of(Kind::Text, &utf16), "季度報告 hello");
    // Binary (zero bytes) and another encoding (bytes that are not UTF-8) are not text
    assert_eq!(extract::read(Kind::Text, b"abc\0def").unwrap_err(), "It is not text");
    let big5 = [0xA7u8, 0xDA, 0xAB, 0x7E].repeat(50);
    assert_eq!(extract::read(Kind::Text, &big5).unwrap_err(), "It is not text");
    assert_eq!(extract::read(Kind::Text, b"  \n ").unwrap_err(), "It holds no text");
}

#[test]
fn the_text_of_a_file_is_cut_at_the_limit_on_a_character() {
    let word = "界".repeat(100);
    let text = std::iter::repeat_n(word.as_str(), 40_000).collect::<Vec<_>>().join(" ");
    let kept = text_of(Kind::Text, text.as_bytes());
    assert!(kept.len() <= extract::MAX_BYTES && kept.len() > extract::MAX_BYTES - 400, "{}", kept.len());
    assert!(kept.is_char_boundary(kept.len()));
}

#[test]
fn web_pages_give_the_text_between_their_tags() {
    let page = "<html><head><title>Roadmap</title><style>p { color: red }</style><script>var secret = 'nope';</script></head>\
                <body><!-- hidden note --><h1>Launch &amp; plan</h1><p>Ship <b>the</b> thing</p></body></html>";
    assert_eq!(text_of(Kind::Markup, page.as_bytes()), "Roadmap Launch & plan Ship the thing");
}

#[tokio::test]
async fn word_documents_give_their_text_with_words_split_over_runs_joined() {
    let body = r#"<w:document xmlns:w="w"><w:body>
        <w:p><w:r><w:t>Quar</w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>terly</w:t></w:r><w:r><w:t xml:space="preserve"> budget </w:t></w:r><w:r><w:t>R&amp;D</w:t></w:r></w:p>
        <w:p><w:r><w:delText>deleted words</w:delText></w:r><w:r><w:instrText>PAGE</w:instrText></w:r><w:r><w:t>Second</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>paragraph</w:t></w:r></w:p>
        </w:body></w:document>"#;
    let docx = zip_of(&[
        ("[Content_Types].xml", "<Types/>"),
        ("word/document.xml", body),
        ("word/footnotes.xml", r#"<w:footnotes xmlns:w="w"><w:p><w:r><w:t>A footnote</w:t></w:r></w:p></w:footnotes>"#),
    ])
    .await;
    assert_eq!(text_of(Kind::Docx, &docx), "Quarterly budget R&D Second paragraph A footnote");
    // A ZIP that is not a Word document is not read as one
    let other = zip_of(&[("readme.txt", "hello")]).await;
    assert!(extract::read(Kind::Docx, &other).is_err());
    assert!(extract::read(Kind::Docx, b"not a zip at all").is_err());
}

#[tokio::test]
async fn workbooks_give_their_shared_strings_and_presentations_their_slides_in_order() {
    let strings = r#"<sst><si><t>Revenue</t></si><si><r><t>North</t></r><r><t xml:space="preserve"> region</t></r></si></sst>"#;
    let xlsx = zip_of(&[
        ("xl/workbook.xml", "<workbook/>"),
        ("xl/sharedStrings.xml", strings),
        ("xl/worksheets/sheet1.xml", "<worksheet><sheetData><row><c><v>42</v></c></row></sheetData></worksheet>"),
    ])
    .await;
    assert_eq!(text_of(Kind::Xlsx, &xlsx), "Revenue North region");
    // A writer that keeps the text in the sheets
    let inline = zip_of(&[
        ("xl/workbook.xml", "<workbook/>"),
        ("xl/worksheets/sheet1.xml", "<worksheet><row><c t=\"inlineStr\"><is><t>Inline cell</t></is></c></row></worksheet>"),
    ])
    .await;
    assert_eq!(text_of(Kind::Xlsx, &inline), "Inline cell");

    let slide = |text: &str| format!(r#"<p:sld xmlns:p="p" xmlns:a="a"><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:sld>"#);
    let (s1, s2, s10, notes) = (slide("First slide"), slide("Second slide"), slide("Tenth slide"), slide("Speaker notes"));
    let pptx = zip_of(&[
        ("ppt/presentation.xml", "<p/>"),
        ("ppt/slides/slide10.xml", &s10),
        ("ppt/slides/slide2.xml", &s2),
        ("ppt/slides/slide1.xml", &s1),
        ("ppt/notesSlides/notesSlide1.xml", &notes),
    ])
    .await;
    // Slide 2 comes before slide 10, whatever order the archive holds them in
    assert_eq!(text_of(Kind::Pptx, &pptx), "First slide Second slide Tenth slide Speaker notes");
}

#[tokio::test]
async fn open_document_files_give_their_text() {
    let content = r#"<office:document-content xmlns:office="o" xmlns:text="t"><office:body><office:text><text:h>Title</text:h><text:p>First <text:span>second</text:span> third</text:p></office:text></office:body></office:document-content>"#;
    let odt = zip_of(&[("mimetype", "application/vnd.oasis.opendocument.text"), ("content.xml", content)]).await;
    assert_eq!(text_of(Kind::Odf, &odt), "Title First second third");
}

#[tokio::test]
async fn an_archive_that_expands_without_end_is_read_up_to_a_limit_and_damaged_ones_are_refused() {
    // 8 MB of the same letter deflates to a few KB: the reader stops at the cap it was given
    let big = "a".repeat(8 * 1024 * 1024);
    let zip = zip_of(&[("word/document.xml", &big)]).await;
    assert!(zip.len() < 100 * 1024, "{}", zip.len());
    let archive = Archive::parse(&zip).unwrap();
    let entry = archive.find("word/document.xml").unwrap();
    assert_eq!(archive.read(entry, 1000).unwrap().len(), 1000);
    assert_eq!(archive.read(entry, usize::MAX / 2).unwrap().len(), big.len());
    // Cut off, or not a ZIP: nothing is read, and nothing panics
    assert!(Archive::parse(&zip[..zip.len() / 2]).is_none());
    assert!(Archive::parse(b"PK\x05\x06 too short").is_none());
    assert!(Archive::parse(&[]).is_none());
    // A directory that points outside the archive
    let mut broken = zip.clone();
    let end = broken.len() - 22;
    broken[end + 16..end + 20].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Archive::parse(&broken).is_none());
}

// ───────────── The job ─────────────

async fn turn_on(env: &testutil::TestEnv, config: Config) {
    let admin = env.admin().await;
    let Json(info) = super::save(State(env.st.clone()), Admin(admin), Json(Config { enabled: true, ..config })).await.unwrap();
    assert!(info.config.enabled);
}

/// Lets the job read everything that is waiting
async fn read_all(env: &testutil::TestEnv) {
    for _ in 0..200 {
        if !indexer::step(&env.st).await.unwrap() {
            return;
        }
    }
    panic!("the job did not finish");
}

/// The files whose text holds `word`, by name
async fn found(env: &testutil::TestEnv, word: &str) -> Vec<String> {
    let expr = super::match_expr(word).unwrap();
    let mut names: Vec<(String,)> = sqlx::query_as(
        "SELECT n.name FROM content_fts CROSS JOIN content_index ci ON ci.fts_rowid = content_fts.rowid JOIN nodes n ON n.id = ci.node_id WHERE content_fts MATCH ?",
    )
    .bind(expr)
    .fetch_all(&env.st.db)
    .await
    .unwrap();
    names.sort();
    names.into_iter().map(|n| n.0).collect()
}

async fn state_of(env: &testutil::TestEnv, id: &str) -> Option<String> {
    sqlx::query_as::<_, (String,)>("SELECT state FROM content_index WHERE node_id = ?").bind(id).fetch_optional(&env.st.db).await.unwrap().map(|r| r.0)
}

async fn queued(env: &testutil::TestEnv) -> i64 {
    sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM content_queue").fetch_one(&env.st.db).await.unwrap().0
}

/// Gives a stored file new content, as a save does
async fn resave(env: &testutil::TestEnv, id: &str, content: &[u8]) {
    let hash = crate::util::sha256_hex(content);
    let tmp = env.dir.join("tmp").join(format!("re-{}", crate::util::new_id()));
    std::fs::write(&tmp, content).unwrap();
    crate::storage::Storage::put_file(env.st.storage("local").unwrap().as_ref(), &hash, &tmp).await.unwrap();
    let mut c = env.st.db.acquire().await.unwrap();
    crate::tree::add_blob_ref(&mut c, &hash, content.len() as i64, "local").await.unwrap();
    sqlx::query("UPDATE nodes SET blob_hash = ?, size = ? WHERE id = ?").bind(&hash).bind(content.len() as i64).bind(id).execute(&mut *c).await.unwrap();
}

#[tokio::test]
async fn nothing_is_queued_or_read_until_it_is_turned_on_and_turning_it_on_queues_what_exists() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.stored_file(&amy, amy.root(), "before.txt", b"quarterly numbers").await;
    assert_eq!(queued(&env).await, 0, "off: nothing is queued");
    read_all(&env).await;
    assert!(found(&env, "quarterly").await.is_empty());

    turn_on(&env, Config::default()).await;
    assert_eq!(queued(&env).await, 1, "what exists is queued");
    // A file added afterwards is queued by the database itself
    env.stored_file(&amy, amy.root(), "after.txt", b"quarterly forecast").await;
    assert_eq!(queued(&env).await, 2);
    read_all(&env).await;
    assert_eq!(queued(&env).await, 0);
    assert_eq!(found(&env, "quarterly").await, ["after.txt", "before.txt"]);
    // Any part of a word, in any letter case
    assert_eq!(found(&env, "FORECA").await, ["after.txt"]);
}

#[tokio::test]
async fn a_saved_file_is_read_again_a_renamed_one_by_its_new_kind_and_the_trash_and_restore_keep_it_searchable_only_while_out_of_it() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    turn_on(&env, Config::default()).await;
    let doc = env.stored_file(&amy, amy.root(), "plan.txt", b"alpha bravo").await;
    let blob = env.stored_file(&amy, amy.root(), "data.bin", b"charlie delta").await;
    read_all(&env).await;
    assert_eq!(found(&env, "alpha").await, ["plan.txt"]);
    assert_eq!(state_of(&env, &blob).await.as_deref(), Some("skipped"), "a kind of file that is not read");
    assert!(found(&env, "charlie").await.is_empty());

    // A save: the old text goes, the new text comes
    resave(&env, &doc, b"echo foxtrot").await;
    assert_eq!(queued(&env).await, 1);
    read_all(&env).await;
    assert!(found(&env, "alpha").await.is_empty());
    assert_eq!(found(&env, "foxtrot").await, ["plan.txt"]);

    // A rename to a kind that is read: read now
    sqlx::query("UPDATE nodes SET name = 'data.txt' WHERE id = ?").bind(&blob).execute(&env.st.db).await.unwrap();
    read_all(&env).await;
    assert_eq!(found(&env, "charlie").await, ["data.txt"]);
    assert_eq!(state_of(&env, &blob).await.as_deref(), Some("indexed"));
    // And to one that is not: the text goes
    sqlx::query("UPDATE nodes SET name = 'data.bin' WHERE id = ?").bind(&blob).execute(&env.st.db).await.unwrap();
    read_all(&env).await;
    assert!(found(&env, "charlie").await.is_empty());

    // Deleting for good takes the text with it, and the row
    sqlx::query("DELETE FROM nodes WHERE id = ?").bind(&doc).execute(&env.st.db).await.unwrap();
    assert!(found(&env, "foxtrot").await.is_empty());
    assert_eq!(state_of(&env, &doc).await, None);
    let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM content_fts").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn a_file_in_the_trash_is_not_read_and_one_restored_is() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    turn_on(&env, Config::default()).await;
    let doc = env.stored_file(&amy, amy.root(), "old.txt", b"golf hotel").await;
    sqlx::query("UPDATE nodes SET trashed_at = unixepoch() WHERE id = ?").bind(&doc).execute(&env.st.db).await.unwrap();
    read_all(&env).await;
    assert_eq!(state_of(&env, &doc).await, None, "not read in the trash");
    assert_eq!(queued(&env).await, 0);
    sqlx::query("UPDATE nodes SET trashed_at = NULL WHERE id = ?").bind(&doc).execute(&env.st.db).await.unwrap();
    assert_eq!(queued(&env).await, 1, "back from the trash: queued");
    read_all(&env).await;
    assert_eq!(found(&env, "hotel").await, ["old.txt"]);
}

#[tokio::test]
async fn files_over_the_limit_are_left_alone_and_the_total_stops_the_job_until_it_is_raised() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    turn_on(&env, Config { max_file_mb: 1, ..Config::default() }).await;
    let big = vec![b'x'; 1024 * 1024 + 10];
    let large = env.stored_file(&amy, amy.root(), "large.txt", &big).await;
    read_all(&env).await;
    assert_eq!(state_of(&env, &large).await.as_deref(), Some("skipped"));

    // A larger limit reads it
    let admin = env.admin().await;
    let save = |max_file_mb, max_total_mb| {
        super::save(State(env.st.clone()), Admin(admin.clone()), Json(Config { enabled: true, max_file_mb, max_total_mb, paused: false }))
    };
    let _ = save(2, 500).await.unwrap();
    read_all(&env).await;
    assert_eq!(state_of(&env, &large).await.as_deref(), Some("indexed"));

    // The text kept in all: the job stops once it is reached, and goes on when the limit is raised
    let _ = save(20, 10).await.unwrap();
    let chunk = "word ".repeat(1_000_000);
    for i in 0..5 {
        env.stored_file(&amy, amy.root(), &format!("part{i}.txt"), chunk.as_bytes()).await;
    }
    read_all(&env).await;
    let Json(info) = super::get(State(env.st.clone()), Admin(admin.clone())).await.unwrap();
    assert_eq!((info.full, info.queued), (true, 1), "indexed {}", info.indexed);
    let _ = save(20, 1000).await.unwrap();
    read_all(&env).await;
    let Json(info) = super::get(State(env.st.clone()), Admin(admin)).await.unwrap();
    assert_eq!((info.indexed, info.queued, info.full), (6, 0, false));
}

#[tokio::test]
async fn pausing_stops_reading_and_asking_again_reads_everything_anew_and_turning_it_off_deletes_the_index() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let admin = env.admin().await;
    turn_on(&env, Config { paused: true, ..Config::default() }).await;
    env.stored_file(&amy, amy.root(), "a.txt", b"india juliet").await;
    read_all(&env).await;
    assert_eq!(queued(&env).await, 1, "paused: waiting");
    assert!(found(&env, "india").await.is_empty());
    let _ = super::save(State(env.st.clone()), Admin(admin.clone()), Json(Config { enabled: true, ..Config::default() })).await.unwrap();
    read_all(&env).await;
    assert_eq!(found(&env, "india").await, ["a.txt"]);

    // Index again: deleted first, then read
    let Json(info) = super::rebuild(State(env.st.clone()), Admin(admin.clone())).await.unwrap();
    assert_eq!((info.indexed, info.queued), (0, 1));
    assert!(found(&env, "india").await.is_empty());
    read_all(&env).await;
    assert_eq!(found(&env, "india").await, ["a.txt"]);

    // Off: everything read is deleted, and nothing is queued any more
    let _ = super::save(State(env.st.clone()), Admin(admin.clone()), Json(Config::default())).await.unwrap();
    assert!(found(&env, "india").await.is_empty());
    assert_eq!(queued(&env).await, 0);
    env.stored_file(&amy, amy.root(), "b.txt", b"kilo lima").await;
    assert_eq!(queued(&env).await, 0);
    assert!(super::rebuild(State(env.st.clone()), Admin(admin)).await.is_err(), "nothing to read again while it is off");
}

#[tokio::test]
async fn settings_are_checked() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    for bad in [Config { max_file_mb: 0, ..Config::default() }, Config { max_file_mb: 101, ..Config::default() }, Config { max_total_mb: 9, ..Config::default() }] {
        assert!(super::save(State(env.st.clone()), Admin(admin.clone()), Json(bad)).await.is_err());
    }
    let Json(info) = super::get(State(env.st.clone()), Admin(admin)).await.unwrap();
    assert!(!info.config.enabled);
}

#[tokio::test]
async fn a_folder_space_file_is_read_and_read_again_when_the_file_on_disk_changes() {
    let env = testutil::env().await;
    let space = env.folder_space("Disk").await;
    turn_on(&env, Config::default()).await;
    testutil::write_old(&space.dir.join("notes.md"), b"# Mike november");
    crate::folders::scan(&env.st, &space.drive).await.unwrap();
    read_all(&env).await;
    assert_eq!(found(&env, "november").await, ["notes.md"]);
    // Changed on the disk: the scan notices, the job reads it again
    let later = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
    std::fs::write(space.dir.join("notes.md"), b"# Mike oscar papa").unwrap();
    std::fs::File::options().write(true).open(space.dir.join("notes.md")).unwrap().set_modified(later).unwrap();
    crate::folders::scan(&env.st, &space.drive).await.unwrap();
    read_all(&env).await;
    assert!(found(&env, "november").await.is_empty());
    assert_eq!(found(&env, "oscar").await, ["notes.md"]);
}

#[tokio::test]
async fn a_file_that_cannot_be_opened_is_tried_again_later_and_then_given_up_on() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    turn_on(&env, Config::default()).await;
    let doc = env.stored_file(&amy, amy.root(), "lost.txt", b"quebec romeo").await;
    // Its content has gone from the storage location
    std::fs::remove_file(testutil::blob_file(&env, b"quebec romeo")).unwrap();
    read_all(&env).await;
    let (attempts, not_before): (i64, i64) =
        sqlx::query_as("SELECT attempts, not_before FROM content_queue WHERE node_id = ?").bind(&doc).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(attempts, 1);
    assert!(not_before > crate::util::now() + 200, "waits before the next try");
    assert_eq!(state_of(&env, &doc).await, None);
    // After the last try it is recorded as failed, and left alone until its content changes
    for _ in 0..4 {
        sqlx::query("UPDATE content_queue SET not_before = 0").execute(&env.st.db).await.unwrap();
        read_all(&env).await;
    }
    assert_eq!(state_of(&env, &doc).await.as_deref(), Some("failed"));
    assert_eq!(queued(&env).await, 0);
}

#[tokio::test]
async fn content_that_changes_while_a_file_is_being_read_stays_queued() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    turn_on(&env, Config::default()).await;
    let doc = env.stored_file(&amy, amy.root(), "race.txt", b"sierra tango").await;
    let old_stamp = crate::util::sha256_hex(b"sierra tango");
    resave(&env, &doc, b"uniform victor").await;
    // The text read from the old content arrives after the new content was saved
    indexer::store(&env.st, &doc, &old_stamp, Outcome::Text("sierra tango".into())).await.unwrap();
    assert_eq!(state_of(&env, &doc).await, None, "the old text is not kept");
    assert_eq!(queued(&env).await, 1, "and the file is still waiting");
    read_all(&env).await;
    assert_eq!(found(&env, "victor").await, ["race.txt"]);
}

#[test]
fn terms_become_expressions_for_the_trigram_index() {
    let e = super::match_expr;
    assert_eq!(e("quarterly").as_deref(), Some("\"quarterly\""));
    assert_eq!(e("quarterly report").as_deref(), Some("\"quarterly\" AND \"report\""));
    // A short word makes the whole term one phrase; under three characters there is nothing to look up
    assert_eq!(e("to do list").as_deref(), Some("\"to do list\""));
    assert_eq!(e("季度報").as_deref(), Some("\"季度報\""));
    assert_eq!(e("季度"), None);
    assert_eq!(e("  "), None);
    // Quotes in a term are doubled, so a term can't become part of the expression
    assert_eq!(e("say \"hi\" now").as_deref(), Some("\"say\" AND \"\"\"hi\"\"\" AND \"now\""));
    assert!(e("a b c d e f g h i j k l").is_some());
}

#[tokio::test]
async fn looking_up_three_thousand_files_takes_a_moment_and_a_snippet_marks_the_match() {
    // A benchmark: storing 3,000 files of 400 words takes a few seconds, looking in them well under one
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    turn_on(&env, Config::default()).await;
    let ids: Vec<String> = {
        let mut ids = Vec::new();
        let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
        for i in 0..3_000 {
            let id = format!("bench{i:05}");
            sqlx::query(
                "INSERT INTO nodes (id, owner_id, parent_id, kind, name, size, mime, drive_id, created_at, updated_at)
                 SELECT ?1, ?2, ?3, 'file', ?4, 1, 'text/plain', drive_id, 1, 1 FROM nodes WHERE id = ?3",
            )
            .bind(&id)
            .bind(amy.id)
            .bind(amy.root())
            .bind(format!("file{i}.txt"))
            .execute(&mut *tx)
            .await
            .unwrap();
            ids.push(id);
        }
        tx.commit().await.unwrap();
        ids
    };
    sqlx::query("DELETE FROM content_queue").execute(&env.st.db).await.unwrap();
    // 3,000 files of about 400 words each; one of them holds the rare word
    for (i, id) in ids.iter().enumerate() {
        let text: String =
            (0..400).map(|w| format!("w{}x{} ", (i * 7 + w * 13) % 5000, w % 11)).collect::<String>() + if i == 2_222 { "needle in the haystack" } else { "" };
        // Files without stored content have this stamp: the size and time of a file on a disk, none of them known
        indexer::store(&env.st, id, "f0-0", Outcome::Text(text)).await.unwrap();
    }
    let started = std::time::Instant::now();
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM content_index").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(n, 3_000);
    assert_eq!(found(&env, "needle").await, ["file2222.txt"]);
    let mut conn = env.st.db.acquire().await.unwrap();
    let snippets = super::snippets(&mut conn, "needle", &ids[2_200..2_300]).await.unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());
    let pieces = &snippets[&ids[2_222]];
    assert_eq!(pieces.len(), 3, "{pieces:?}");
    assert_eq!(pieces[1], "needle");
    assert!(pieces[2].starts_with(" in the haystack"));
    assert_eq!(snippets.len(), 1, "only the file that holds it");
}
