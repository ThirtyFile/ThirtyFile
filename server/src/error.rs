use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub message: String,
    pub code: Option<&'static str>,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into(), code: None }
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
    pub fn internal(e: impl std::fmt::Display) -> Self {
        tracing::error!("internal error: {e}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "A server error occurred")
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = match self.code {
            Some(code) => json!({ "error": self.message, "code": code }),
            None => json!({ "error": self.message }),
        };
        (self.status, Json(body)).into_response()
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

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        if let Some(se) = e.get_ref().and_then(|inner| inner.downcast_ref::<crate::storage::StorageError>()) {
            tracing::warn!("{se}");
            crate::locations::request_recheck();
            return Self::new(StatusCode::SERVICE_UNAVAILABLE, se.message);
        }
        Self::internal(e)
    }
}

impl From<tokio::task::JoinError> for AppError {
    fn from(e: tokio::task::JoinError) -> Self {
        Self::internal(e)
    }
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
        env.folder(&amy, &amy.root_id, "Docs").await;
        let other = env.file(&amy, &amy.root_id, "other").await;
        let e = clash("UPDATE nodes SET name = 'DOCS' WHERE id = ?", other).await;
        assert_eq!((e.status, e.message.as_str()), (StatusCode::CONFLICT, "An item with the same name already exists"));
    }
}
