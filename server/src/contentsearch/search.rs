//! Looking in the text read from files: the match expression for a search term, and the snippets shown with the items a
//! search returns.

use std::collections::HashMap;

use sqlx::SqliteConnection;

use crate::{db, error::AppResult};

use super::{Config, SETTING};

/// Marks around a match in a snippet, which no text read from a file holds (the squashed text has no control characters
/// but white space, and that is gone)
const OPEN: char = '\u{1}';
const CLOSE: char = '\u{2}';
/// Words of a term that are looked for at most
const MAX_WORDS: usize = 8;
/// Snippets are made for this many items at most per search (a search returns 300)
const SNIPPETS: usize = 300;

/// Whether searching inside files is turned on
pub async fn enabled(conn: &mut SqliteConnection) -> AppResult<bool> {
    Ok(db::json_setting::<Config>(conn, SETTING).await?.enabled)
}

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// The FTS5 expression for a term, or None when it is too short for the trigram index (it needs three characters). With
/// several words, a file matches when it holds all of them, wherever they are; a word of less than three characters
/// then takes the whole term to be one phrase instead.
pub fn match_expr(term: &str) -> Option<String> {
    // As the text of files is kept
    let term = super::extract::compatible(term);
    let term = term.as_str();
    let words: Vec<&str> = term.split_whitespace().take(MAX_WORDS).collect();
    if !words.is_empty() && words.iter().all(|w| w.chars().count() >= 3) {
        return Some(words.iter().map(|w| quoted(w)).collect::<Vec<_>>().join(" AND "));
    }
    let phrase = term.split_whitespace().collect::<Vec<_>>().join(" ");
    (phrase.chars().count() >= 3).then(|| quoted(&phrase))
}

/// A snippet as the pieces around the matches: the text before, a match, the text between, the next match, and so on
/// (odd positions are matches)
fn pieces(marked: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    for c in marked.chars() {
        match c {
            OPEN | CLOSE => out.push(String::new()),
            _ => out.last_mut().expect("starts with one piece").push(c),
        }
    }
    out
}

/// The snippets of `ids` (items the person was already shown to be allowed to see, and found in the list the search
/// returns) for `term`, by item. Items found by name only have none.
pub async fn snippets(conn: &mut SqliteConnection, term: &str, ids: &[String]) -> AppResult<HashMap<String, Vec<String>>> {
    let Some(expr) = match_expr(term) else {
        return Ok(HashMap::new());
    };
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let ids = serde_json::to_string(&ids[..ids.len().min(SNIPPETS)]).unwrap_or_default();
    // Driven by the items, then the index is asked about each row: a common word is not looked up in every file. A
    // snippet is 64 tokens, the most FTS5 allows: trigrams, so about 66 characters.
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT ci.node_id, snippet(content_fts, 0, char(1), char(2), '…', 64)
         FROM content_index ci CROSS JOIN content_fts ON content_fts.rowid = ci.fts_rowid
         WHERE ci.node_id IN (SELECT value FROM json_each(?1)) AND content_fts MATCH ?2",
    )
    .bind(ids)
    .bind(expr)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|(id, marked)| (id, pieces(&marked))).collect())
}
