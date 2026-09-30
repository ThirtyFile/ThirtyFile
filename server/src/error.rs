use std::sync::{Arc, Mutex};

use axum::{
    Json,
    extract::{MatchedPath, Request},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub message: String,
    pub code: Option<&'static str>,
    /// For server failures: what kind of failure, for the error log (never names, paths or contents; see `logs::errors`)
    pub diag: Option<String>,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into(), code: None, diag: None }
    }
    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }
    pub fn bad_request(m: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, m)
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "Please sign in")
    }
    pub fn forbidden(m: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, m)
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, m)
    }
    pub fn conflict(m: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, m)
    }
    /// An unexpected failure: its text goes to the server log under the request's id; the person is told a server error
    /// occurred, and the error log records what kind of failure it was (the text can name files and paths)
    pub fn internal<E: std::fmt::Display + 'static>(e: E) -> Self {
        let request = request_id().unwrap_or_default();
        tracing::error!(request, "internal error: {e}");
        let mut err = Self::new(StatusCode::INTERNAL_SERVER_ERROR, "A server error occurred");
        err.diag = Some(diagnosis(&e));
        err
    }
}

/// What kind of failure an error is, without its text: SQLite's messages name tables and columns (never values) and
/// fixed wording (`&'static str`) names nothing; other text can name files, folders and hosts
fn diagnosis<E: std::fmt::Display + 'static>(e: &E) -> String {
    let any = e as &dyn std::any::Any;
    if let Some(io) = any.downcast_ref::<std::io::Error>() {
        return match io.raw_os_error() {
            Some(code) => format!("Disk or storage error: {:?} (os error {code})", io.kind()),
            None => format!("Disk or storage error: {:?}", io.kind()),
        };
    }
    if let Some(db) = any.downcast_ref::<sqlx::Error>() {
        return format!("Database error: {}", crate::redact::redact(&db.to_string()));
    }
    if let Some(text) = any.downcast_ref::<&str>() {
        return text.to_string();
    }
    if any.is::<tokio::task::JoinError>() {
        return "A background task failed".into();
    }
    format!("{} (the text is in the server log)", std::any::type_name::<E>())
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let info = self.info();
        let body = match self.code {
            Some(code) => json!({ "error": self.message, "code": code }),
            None => json!({ "error": self.message }),
        };
        let mut res = (self.status, Json(body)).into_response();
        res.extensions_mut().insert(info);
        res
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db) = &e
            && db.is_unique_violation()
        {
            return Self::conflict(unique_violation(db.message()));
        }
        Self::internal(e)
    }
}

/// What a unique constraint violation means, by the table it is on. SQLite names the columns ("UNIQUE constraint failed:
/// users.username"), or the index when it is on an expression ("... failed: index 'nodes_name_uq'"). Callers that
/// expect a clash with something more specific check for it themselves.
fn unique_violation(message: &str) -> &'static str {
    let what = message.rsplit_once("failed: ").map_or("", |(_, w)| w).trim_start_matches("index '");
    let table = what.split(['.', '\'']).next().unwrap_or_default();
    match table {
        "nodes" | "nodes_name_uq" => "An item with the same name already exists",
        "users" => "Username already exists",
        "groups" => "A group with this name already exists",
        _ => "This already exists",
    }
}

impl From<tokio::task::JoinError> for AppError {
    fn from(e: tokio::task::JoinError) -> Self {
        Self::internal(e)
    }
}

impl AppError {
    /// What this error response tells the error log
    pub(crate) fn info(&self) -> ErrorInfo {
        ErrorInfo { message: self.message.clone(), code: self.code, diag: self.diag.clone() }
    }
}

// ───────────── The request being answered ─────────────

tokio::task_local! {
    pub(crate) static REQUEST: Arc<RequestCtx>;
}

/// What is known about the request being answered: its id, and who sent it once that is known
pub struct RequestCtx {
    pub(crate) id: String,
    pub(crate) user: Mutex<Option<(i64, String)>>,
    pub(crate) route: Mutex<Option<String>>,
}

/// The id of the request being answered (none outside a request, e.g. in a background task)
pub fn request_id() -> Option<String> {
    REQUEST.try_with(|c| c.id.clone()).ok()
}

/// Notes who sent the request being answered (called when the session is recognised)
pub fn note_user(id: i64, username: &str) {
    let _ = REQUEST.try_with(|c| *c.user.lock().unwrap() = Some((id, username.to_string())));
}

/// Notes the route template that answers the request (a `route_layer`: routing is done by then)
pub async fn note_route(req: Request, next: Next) -> Response {
    if let Some(p) = req.extensions().get::<MatchedPath>() {
        let p = p.as_str().to_string();
        let _ = REQUEST.try_with(|c| *c.route.lock().unwrap() = Some(p));
    }
    next.run(req).await
}

/// What an error response says, for the error log (a response extension added by `AppError`)
#[derive(Clone, Debug)]
pub struct ErrorInfo {
    pub message: String,
    pub code: Option<&'static str>,
    /// For server failures: what kind of failure (never names, paths or contents)
    pub diag: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[test]
    fn unique_violations_are_named_by_their_table() {
        assert_eq!(unique_violation("UNIQUE constraint failed: index 'nodes_name_uq'"), "An item with the same name already exists");
        assert_eq!(unique_violation("UNIQUE constraint failed: nodes.parent_id, nodes.name_key"), "An item with the same name already exists");
        assert_eq!(unique_violation("UNIQUE constraint failed: users.username"), "Username already exists");
        assert_eq!(unique_violation("UNIQUE constraint failed: groups.name"), "A group with this name already exists");
        assert_eq!(unique_violation("UNIQUE constraint failed: node_versions.id"), "This already exists");
        assert_eq!(unique_violation("something else"), "This already exists");
    }

    #[tokio::test]
    async fn database_unique_violations_get_the_message_of_their_table() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let clash = |sql: &'static str, id: String| {
            let db = env.st.db.clone();
            async move { AppError::from(sqlx::query(sql).bind(id).execute(&db).await.unwrap_err()) }
        };
        let e = clash("UPDATE users SET username = 'AMY' WHERE id = ?", ben.id.to_string()).await;
        assert_eq!((e.status, e.message.as_str()), (StatusCode::CONFLICT, "Username already exists"));
        env.folder(&amy, amy.root(), "Docs").await;
        let other = env.file(&amy, amy.root(), "other").await;
        let e = clash("UPDATE nodes SET name = 'DOCS' WHERE id = ?", other).await;
        assert_eq!((e.status, e.message.as_str()), (StatusCode::CONFLICT, "An item with the same name already exists"));
    }
}
