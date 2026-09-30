//! The error log: errors people ran into, for administrators (Control panel > Activity > Errors).
//!
//! - The server records the requests it answered with an error (`track`, a layer around every request): unexpected
//!   failures (5xx) as errors, and refused changes (validation, permission, conflict: 4xx to requests that change
//!   something) as warnings. Every error response carries an `X-Request-Id`.
//! - The web page reports what went wrong on it (`client_report`): rendering errors, uncaught errors, rejected promises
//!   and failures it showed the person. A report of a failed request carries its request id and is added to the
//!   server's record of it, rather than recorded a second time.
//!
//! Records are queued and written by the background log writer, in their own transaction, so a failed operation that
//! rolls back keeps its record, and a record that can't be written never fails the request. The same error again
//! within `REPEAT_WINDOW` is counted on its record instead of adding another; ingestion is rate-limited.
//!
//! Privacy: nothing names what is in a space. Routes are the route templates (`/api/nodes/{id}`), never the path asked
//! for; quoted names in messages are left out (`redact`); server failures record the kind of error, and the full text
//! stays in the server log under the request id. No request or response bodies, cookies, tokens or passwords are kept.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Json,
    extract::{ConnectInfo, FromRequestParts, Query, Request, State},
    http::{HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{LogEvent, enqueue};
use crate::{
    auth::{Admin, User},
    error::{AppResult, ErrorInfo, REQUEST, RequestCtx},
    redact::{clip, page_route, redact},
    state::AppState,
    util::now,
};

/// The same error again within this many seconds is counted on the earlier record
pub(super) const REPEAT_WINDOW: i64 = 600;
/// Records the server may queue per minute; more are dropped (and counted in the server log)
const SERVER_PER_MINUTE: u32 = 300;
/// Reports a page may send per minute, per person (or per address when not signed in)
const CLIENT_PER_MINUTE: u32 = 20;
/// Reports from people who aren't signed in, per minute in all
const ANONYMOUS_PER_MINUTE: u32 = 60;
/// Largest report the page may send (bytes)
pub const MAX_REPORT: usize = 16 * 1024;
/// Failed requests remembered so the page's report of one finds the server's record
const REMEMBERED_REQUESTS: usize = 5000;

const MAX_MESSAGE: usize = 500;
const MAX_DETAIL: usize = 4000;

/// Around every request: gives it an id, and records the error when it is answered with one worth recording
pub async fn track(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let ctx = Arc::new(RequestCtx { id: crate::util::random_token(16).to_lowercase(), user: Mutex::new(None), route: Mutex::new(None) });
    let (method, path) = (req.method().clone(), req.uri().path().to_string());
    let mut res = REQUEST.scope(ctx.clone(), next.run(req)).await;
    let status = res.status();
    if status.is_client_error() || status.is_server_error() {
        if let Ok(v) = HeaderValue::from_str(&ctx.id) {
            res.headers_mut().insert("x-request-id", v);
        }
        let route = ctx.route.lock().unwrap().clone();
        if let Some((severity, kind)) = classify(&method, &path, route.as_deref(), status) {
            let info = res.extensions().get::<ErrorInfo>().cloned();
            record_server(&st, &ctx, &method, &path, route, status, info, severity, kind);
        }
    }
    res
}

/// Whether an error response is recorded, and as what: (severity, kind). Unexpected failures always; refusals only of
/// requests that change something (a page that can't load something reports it itself), leaving out sign-in (the
/// sign-in log has it), upload transfers (their chunks are retried and resumed) and WebDAV clients' probing.
pub(super) fn classify(method: &Method, path: &str, route: Option<&str>, status: StatusCode) -> Option<(&'static str, &'static str)> {
    if path == "/api/client-errors" {
        return None;
    }
    if status.is_server_error() {
        let kind = match status {
            StatusCode::SERVICE_UNAVAILABLE => "storage",
            StatusCode::GATEWAY_TIMEOUT => "timeout",
            _ => "server",
        };
        return Some(("error", kind));
    }
    if !path.starts_with("/api/") || matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) || status == StatusCode::UNAUTHORIZED {
        return None;
    }
    let route = route.unwrap_or(path);
    let sign_in = route.starts_with("/api/auth/login") || route.starts_with("/api/auth/reset") || route.ends_with("/unlock");
    let transfer = route.contains("/uploads/{id}") && *method != Method::POST;
    if sign_in || transfer {
        return None;
    }
    let kind = match status {
        StatusCode::FORBIDDEN => "permission",
        StatusCode::NOT_FOUND | StatusCode::GONE => "not_found",
        StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED | StatusCode::LOCKED => "conflict",
        StatusCode::TOO_MANY_REQUESTS | StatusCode::PAYLOAD_TOO_LARGE | StatusCode::INSUFFICIENT_STORAGE => "limit",
        _ => "validation",
    };
    Some(("warning", kind))
}

/// The id in a request's path where its route has `{id}` (an item, a space, an account): an id, never a name or a token
fn resource_of(route: Option<&str>, path: &str) -> String {
    let Some(route) = route else { return String::new() };
    route
        .split('/')
        .zip(path.split('/'))
        .find(|(r, _)| *r == "{id}")
        .map(|(_, p)| p)
        .filter(|p| !p.is_empty() && p.len() <= 64 && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        .unwrap_or_default()
        .to_string()
}

#[allow(clippy::too_many_arguments)]
fn record_server(st: &AppState, ctx: &RequestCtx, method: &Method, path: &str, route: Option<String>, status: StatusCode, info: Option<ErrorInfo>, severity: &'static str, kind: &'static str) {
    if !st.error_log.allow_server() {
        return;
    }
    let resource = resource_of(route.as_deref(), path);
    let route = route.unwrap_or_else(|| if path == "/dav" || path.starts_with("/dav/") { "/dav/…".into() } else { "(no route)".into() });
    let (user_id, username) = ctx.user.lock().unwrap().clone().map_or((None, String::new()), |(id, name)| (Some(id), name));
    let message = match &info {
        Some(i) => redact(&i.message),
        None if status == StatusCode::GATEWAY_TIMEOUT => "The request took too long".to_string(),
        None if status == StatusCode::INTERNAL_SERVER_ERROR => "The request stopped unexpectedly".to_string(),
        None => status.canonical_reason().unwrap_or("Error").to_string(),
    };
    let e = ErrorEvent {
        at: now(),
        source: "backend",
        severity,
        kind: kind.to_string(),
        user_id,
        username,
        operation: format!("{method} {route}"),
        route,
        resource,
        status: Some(status.as_u16() as i64),
        code: info.as_ref().and_then(|i| i.code).unwrap_or_default().to_string(),
        message: clip(&message, MAX_MESSAGE),
        detail: info.and_then(|i| i.diag).map(|d| clip(&d, MAX_DETAIL)).unwrap_or_default(),
        request_id: Some(ctx.id.clone()),
        client: String::new(),
        version: crate::VERSION.to_string(),
        fingerprint: String::new(),
    }
    .fingerprinted();
    st.error_log.remember(&ctx.id, &e.fingerprint);
    enqueue(st, LogEvent::Error(Box::new(e)));
}

// ───────────── Records ─────────────

/// One error, queued for the background log writer
#[derive(Debug, Clone)]
pub struct ErrorEvent {
    pub at: i64,
    pub source: &'static str,
    pub severity: &'static str,
    pub kind: String,
    pub user_id: Option<i64>,
    pub username: String,
    pub operation: String,
    pub route: String,
    pub resource: String,
    pub status: Option<i64>,
    pub code: String,
    pub message: String,
    pub detail: String,
    pub request_id: Option<String>,
    /// A page's report of a failed request: what it said, added to the server's record of that request
    pub client: String,
    pub version: String,
    pub fingerprint: String,
}

impl ErrorEvent {
    /// Sets the fingerprint: the same source, kind, person, operation, status and message are the same error
    fn fingerprinted(mut self) -> Self {
        let key = format!("{}|{}|{}|{}|{:?}|{}", self.source, self.kind, self.user_id.unwrap_or(0), self.operation, self.status, self.message);
        self.fingerprint = crate::util::sha256_hex(key.as_bytes())[..32].to_string();
        self
    }
}

/// Writes one error (by the background log writer, within its batch's transaction): adds the page's report to the
/// server's record of the same request, or counts it on a recent record of the same error, or adds a record
pub(super) async fn write_error(conn: &mut sqlx::SqliteConnection, st: &AppState, e: &ErrorEvent) -> Result<(), sqlx::Error> {
    if !e.client.is_empty()
        && let Some(request) = &e.request_id
    {
        // The server's record carries the request's id, or had it before a later occurrence of the same error took over
        let fingerprint = st.error_log.fingerprint_of(request);
        let res = sqlx::query(
            "UPDATE error_log SET client = ? WHERE id = (SELECT id FROM error_log WHERE source = 'backend'
               AND (request_id = ? OR (fingerprint = ? AND at >= ?)) ORDER BY id DESC LIMIT 1)",
        )
        .bind(&e.client)
        .bind(request)
        .bind(fingerprint.unwrap_or_default())
        .bind(e.at - REPEAT_WINDOW)
        .execute(&mut *conn)
        .await?;
        if res.rows_affected() > 0 {
            return Ok(());
        }
    }
    let res = sqlx::query(
        "UPDATE error_log SET count = count + 1, at = MAX(at, ?), request_id = COALESCE(?, request_id)
         WHERE id = (SELECT id FROM error_log WHERE fingerprint = ? AND at >= ? ORDER BY id DESC LIMIT 1)",
    )
    .bind(e.at)
    .bind(&e.request_id)
    .bind(&e.fingerprint)
    .bind(e.at - REPEAT_WINDOW)
    .execute(&mut *conn)
    .await?;
    if res.rows_affected() > 0 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO error_log (at, first_at, source, severity, kind, user_id, username, operation, route, resource, status, code,
           message, detail, request_id, client, version, fingerprint)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(e.at)
    .bind(e.at)
    .bind(e.source)
    .bind(e.severity)
    .bind(&e.kind)
    .bind(e.user_id)
    .bind(&e.username)
    .bind(&e.operation)
    .bind(&e.route)
    .bind(&e.resource)
    .bind(e.status)
    .bind(&e.code)
    .bind(&e.message)
    .bind(&e.detail)
    .bind(&e.request_id)
    // A report that found no server record keeps what it said as its own message
    .bind("")
    .bind(&e.version)
    .bind(&e.fingerprint)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

// ───────────── Rate limits and remembered requests ─────────────

/// In memory: the rate limits of the current minute, and recent failed requests
#[derive(Default)]
pub struct ErrorLogState {
    limits: Mutex<Limits>,
    requests: Mutex<HashMap<String, (String, i64)>>,
}

#[derive(Default)]
struct Limits {
    minute: i64,
    server: u32,
    anonymous: u32,
    clients: HashMap<String, u32>,
    dropped: u64,
}

impl Limits {
    /// Starts the counts again each minute (so the per-client map never outlives a minute)
    fn roll(&mut self) {
        let minute = now() / 60;
        if minute != self.minute {
            *self = Limits { minute, dropped: self.dropped, ..Default::default() };
        }
    }

    fn drop_one(&mut self) {
        self.dropped += 1;
        if self.dropped == 1 || self.dropped.is_multiple_of(1000) {
            tracing::warn!("Error log: {} records dropped by the rate limit so far", self.dropped);
        }
    }
}

impl ErrorLogState {
    fn allow_server(&self) -> bool {
        let mut l = self.limits.lock().unwrap();
        l.roll();
        if l.server >= SERVER_PER_MINUTE {
            l.drop_one();
            return false;
        }
        l.server += 1;
        true
    }

    /// Whether a page's report is accepted: per person, or per address (and all such together) when not signed in
    fn allow_client(&self, key: &str, anonymous: bool) -> bool {
        let mut l = self.limits.lock().unwrap();
        l.roll();
        let n = l.clients.get(key).copied().unwrap_or(0);
        if n >= CLIENT_PER_MINUTE || (anonymous && l.anonymous >= ANONYMOUS_PER_MINUTE) {
            l.drop_one();
            return false;
        }
        l.clients.insert(key.to_string(), n + 1);
        if anonymous {
            l.anonymous += 1;
        }
        true
    }

    fn remember(&self, request: &str, fingerprint: &str) {
        let mut m = self.requests.lock().unwrap();
        if m.len() >= REMEMBERED_REQUESTS {
            let cutoff = now() - REPEAT_WINDOW;
            m.retain(|_, (_, at)| *at >= cutoff);
            if m.len() >= REMEMBERED_REQUESTS {
                return;
            }
        }
        m.insert(request.to_string(), (fingerprint.to_string(), now()));
    }

    fn fingerprint_of(&self, request: &str) -> Option<String> {
        self.requests.lock().unwrap().get(request).map(|(f, _)| f.clone())
    }
}

// ───────────── Reports from the page ─────────────

#[derive(Deserialize)]
pub struct ClientReport {
    /// render, uncaught, rejection or handled (a failure the page showed)
    kind: String,
    /// What was being done: upload, preview, rename…
    #[serde(default)]
    operation: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    stack: String,
    /// The page (its path, without the query)
    #[serde(default)]
    route: String,
    /// The failed request's X-Request-Id
    request_id: Option<String>,
    status: Option<i64>,
    #[serde(default)]
    code: String,
    /// The id of the item concerned
    #[serde(default)]
    resource: String,
    /// Which build of the page sent it
    #[serde(default)]
    build: String,
}

/// A page reports an error. Who reported it comes from the session, never from the report; everything in it is
/// treated as untrusted text: checked, shortened and redacted. Answers whether it was accepted (`recorded`).
pub async fn client_report(State(st): State<AppState>, ConnectInfo(addr): ConnectInfo<SocketAddr>, req: Request) -> Response {
    let (mut parts, body) = req.into_parts();
    let user = User::from_request_parts(&mut parts, &st).await.ok();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_REPORT).await else {
        return refused(StatusCode::PAYLOAD_TOO_LARGE);
    };
    let Ok(r) = serde_json::from_slice::<ClientReport>(&bytes) else {
        return refused(StatusCode::BAD_REQUEST);
    };
    let Some(e) = client_event(r, user.as_ref()) else {
        return refused(StatusCode::BAD_REQUEST);
    };
    let key = match &user {
        Some(u) => format!("user:{}", u.id),
        None => format!("ip:{}", crate::auth::limit_key_ip(&crate::auth::client_ip(&st, addr, &parts.headers))),
    };
    if !st.error_log.allow_client(&key, user.is_none()) {
        return refused(StatusCode::TOO_MANY_REQUESTS);
    }
    enqueue(&st, LogEvent::Error(Box::new(e)));
    (StatusCode::ACCEPTED, Json(json!({ "recorded": true }))).into_response()
}

fn refused(status: StatusCode) -> Response {
    (status, Json(json!({ "recorded": false }))).into_response()
}

/// The record for a page's report, or None when it isn't one
pub(super) fn client_event(r: ClientReport, user: Option<&User>) -> Option<ErrorEvent> {
    let kind = match r.kind.as_str() {
        k @ ("render" | "uncaught" | "rejection" | "handled") => k,
        _ => return None,
    };
    let status = r.status.filter(|s| (100..=599).contains(s));
    // A failure the page handled and showed is expected when the server refused (4xx); anything else is unexpected
    let severity = if kind == "handled" && status.is_some_and(|s| (400..500).contains(&s)) { "warning" } else { "error" };
    let operation = word(&r.operation, 40);
    let message = clip(&redact(r.message.trim()), MAX_MESSAGE);
    let request_id = r.request_id.filter(|id| !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_alphanumeric()));
    let build = word(&r.build, 60);
    let version = if build.is_empty() { crate::VERSION.to_string() } else { format!("{} (web {build})", crate::VERSION) };
    let client = if request_id.is_some() { clip(&format!("{}: {message}", if operation.is_empty() { kind } else { &operation }), MAX_MESSAGE) } else { String::new() };
    Some(
        ErrorEvent {
            at: now(),
            source: "frontend",
            severity,
            kind: kind.to_string(),
            user_id: user.map(|u| u.id),
            username: user.map(|u| u.username.clone()).unwrap_or_default(),
            operation,
            route: page_route(&r.route),
            resource: word(&r.resource, 64),
            status,
            code: word(&r.code, 60),
            message,
            detail: clip(&redact(&r.stack), MAX_DETAIL),
            request_id,
            client,
            version,
            fingerprint: String::new(),
        }
        .fingerprinted(),
    )
}

/// A short identifier-like value (letters, digits, `-`, `_`, `.`), or nothing
fn word(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')) { s.to_string() } else { String::new() }
}

// ───────────── Viewing ─────────────

#[derive(Serialize, sqlx::FromRow)]
pub struct ErrorRow {
    pub(super) id: i64,
    pub(super) at: i64,
    pub(super) first_at: i64,
    pub(super) count: i64,
    pub(super) source: String,
    pub(super) severity: String,
    pub(super) kind: String,
    pub(super) user_id: Option<i64>,
    pub(super) username: String,
    pub(super) operation: String,
    pub(super) route: String,
    pub(super) resource: String,
    pub(super) status: Option<i64>,
    pub(super) code: String,
    pub(super) message: String,
    pub(super) detail: String,
    pub(super) request_id: Option<String>,
    pub(super) client: String,
    pub(super) version: String,
}

#[derive(Deserialize, Default)]
pub struct ErrorQuery {
    /// backend, frontend
    pub(super) source: Option<String>,
    /// error, warning (comma-separated)
    pub(super) severity: Option<String>,
    /// Username (partial match)
    pub(super) user: Option<String>,
    /// Keyword: message, operation, route or request id
    pub(super) q: Option<String>,
    pub(super) from: Option<i64>,
    pub(super) to: Option<i64>,
    pub(super) before: Option<i64>,
    pub(super) limit: Option<i64>,
    pub(super) tz: Option<i64>,
}

const ERROR_COLS: &str =
    "id, at, first_at, count, source, severity, kind, user_id, username, operation, route, resource, status, code, message, detail, request_id, client, version";

pub(super) async fn query_errors(st: &AppState, q: &ErrorQuery, limit: i64) -> AppResult<Vec<ErrorRow>> {
    let mut f = super::query::Filters::new(&format!("SELECT {ERROR_COLS} FROM error_log"));
    f.one_of("source", q.source.as_deref())
        .one_of("severity", q.severity.as_deref())
        .contains(&["username"], q.user.as_deref())
        .between("at", q.from, q.to)
        .contains(&["message", "operation", "route", "request_id", "client"], q.q.as_deref());
    f.fetch(&st.db, "id", q.before, limit).await
}

/// The error log, for administrators
pub async fn error_log(State(st): State<AppState>, _: Admin, Query(q): Query<ErrorQuery>) -> AppResult<Json<Value>> {
    let limit = super::query::page_size(q.limit);
    Ok(super::query::page(query_errors(&st, &q, limit).await?, limit, |r| r.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{error::AppError, testutil};
    use axum::{Router, body::Body, routing::post};
    use tower::ServiceExt;

    #[test]
    fn only_errors_worth_recording_are_recorded() {
        let c = |m: Method, path: &str, route: Option<&str>, s: u16| classify(&m, path, route, StatusCode::from_u16(s).unwrap());
        assert_eq!(c(Method::GET, "/api/nodes/x", Some("/api/nodes/{id}"), 500), Some(("error", "server")));
        assert_eq!(c(Method::GET, "/api/files/x/content", None, 503), Some(("error", "storage")));
        assert_eq!(c(Method::from_bytes(b"PROPFIND").unwrap(), "/dav/a", None, 504), Some(("error", "timeout")));
        assert_eq!(c(Method::POST, "/api/folders", Some("/api/folders"), 409), Some(("warning", "conflict")));
        assert_eq!(c(Method::PATCH, "/api/nodes/x", Some("/api/nodes/{id}"), 403), Some(("warning", "permission")));
        assert_eq!(c(Method::POST, "/api/folders", Some("/api/folders"), 400), Some(("warning", "validation")));
        // Reads, sign-ins, upload transfers, WebDAV, signed-out requests and the reports themselves aren't
        assert_eq!(c(Method::GET, "/api/nodes/x", Some("/api/nodes/{id}"), 404), None);
        assert_eq!(c(Method::POST, "/api/auth/login", Some("/api/auth/login"), 403), None);
        assert_eq!(c(Method::POST, "/api/public/shares/t/unlock", Some("/api/public/shares/{token}/unlock"), 403), None);
        assert_eq!(c(Method::PATCH, "/api/uploads/u", Some("/api/uploads/{id}"), 409), None);
        assert_eq!(c(Method::POST, "/api/uploads", Some("/api/uploads"), 413), Some(("warning", "limit")));
        assert_eq!(c(Method::DELETE, "/dav/a", None, 403), None);
        assert_eq!(c(Method::POST, "/api/folders", Some("/api/folders"), 401), None);
        assert_eq!(c(Method::POST, "/api/client-errors", None, 500), None);
        assert_eq!(resource_of(Some("/api/nodes/{id}"), "/api/nodes/0123abc"), "0123abc");
        assert_eq!(resource_of(Some("/api/public/shares/{token}"), "/api/public/shares/secret"), "");
    }

    fn report(v: Value) -> ClientReport {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn reports_are_checked_shortened_and_redacted() {
        let e = client_event(
            report(json!({ "kind": "render", "message": format!("\"secret.txt\" {}", "x".repeat(2000)), "stack": "y".repeat(10_000),
                "route": "/share/tok/abc", "operation": "preview<script>", "build": "index-AbC.js", "request_id": "not valid!" })),
            None,
        )
        .unwrap();
        assert!(e.message.starts_with("\"…\" ") && e.message.chars().count() == MAX_MESSAGE + 1);
        assert_eq!(e.detail.chars().count(), MAX_DETAIL + 1);
        assert_eq!((e.route.as_str(), e.operation.as_str(), e.request_id.as_deref()), ("/share/…/abc", "", None));
        assert_eq!((e.severity, e.user_id, e.username.as_str()), ("error", None, ""));
        assert!(e.version.ends_with("(web index-AbC.js)"));
        assert!(client_event(report(json!({ "kind": "anything", "message": "x" })), None).is_none());
        // A failure the page showed after the server refused is a warning; its report is joined to the server's record
        let e = client_event(report(json!({ "kind": "handled", "operation": "upload", "status": 403, "request_id": "abc123", "message": "No" })), None).unwrap();
        assert_eq!((e.severity, e.client.as_str()), ("warning", "upload: No"));
    }

    async fn rows(st: &AppState, n: usize) -> Vec<ErrorRow> {
        for _ in 0..100 {
            let rows = query_errors(st, &ErrorQuery::default(), 100).await.unwrap();
            if rows.len() >= n && (n > 0 || rows.is_empty()) {
                return rows;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the error log didn't reach {n} rows");
    }

    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }

    /// A router with the error log's layers around a route that writes, then fails (so its transaction rolls back)
    fn failing_app(st: AppState) -> Router {
        let handler = |State(st): State<AppState>, user: User| async move {
            let _w = st.write_lock.lock().await;
            let mut tx = crate::db::begin_write(&st.db).await?;
            sqlx::query("INSERT INTO settings (key, value) VALUES ('rolled-back', 'x')").execute(&mut *tx).await?;
            let _ = user;
            Err::<(), _>(AppError::internal(std::io::Error::other("/srv/storage/amy/Salaries.xlsx is broken")))
        };
        let refuse = |_: User| async { Err::<(), _>(AppError::conflict("An item named \"Plans.docx\" already exists")) };
        let api = Router::new()
            .route("/api/items/{id}/fail", post(handler))
            .route("/api/items/{id}/refuse", post(refuse))
            .route("/api/client-errors", post(client_report))
            .route_layer(axum::middleware::from_fn(crate::error::note_route));
        api.layer(axum::middleware::from_fn_with_state(st.clone(), track)).with_state(st)
    }

    async fn send(app: &Router, uri: &str, cookie: &str, body: Option<Value>) -> Response {
        let req = axum::http::Request::post(uri)
            .header(axum::http::header::COOKIE, cookie)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .extension(ConnectInfo(SocketAddr::from(([203, 0, 113, 5], 4000))))
            .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
            .unwrap();
        app.clone().oneshot(req).await.unwrap()
    }

    #[tokio::test]
    async fn a_failed_request_is_recorded_once_with_its_user_and_request_id_even_when_it_rolled_back() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (_, cookie) = env.sign_in(&amy, "Test").await;
        let app = failing_app(env.st.clone());

        let res = send(&app, "/api/items/0123abcd/fail", &cookie, None).await;
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let request = res.headers()["x-request-id"].to_str().unwrap().to_string();
        let r = &rows(&env.st, 1).await[0];
        assert_eq!((r.source.as_str(), r.severity.as_str(), r.kind.as_str()), ("backend", "error", "server"));
        assert_eq!((r.user_id, r.username.as_str()), (Some(amy.id), "amy"));
        assert_eq!((r.operation.as_str(), r.resource.as_str(), r.status), ("POST /api/items/{id}/fail", "0123abcd", Some(500)));
        assert_eq!(r.request_id.as_deref(), Some(request.as_str()));
        // The kind of failure, not its text (which names a file); that stays in the server log
        assert!(r.detail.contains("NotFound") || r.detail.contains("Other"), "{}", r.detail);
        assert!(!r.detail.contains("Salaries") && !r.message.contains("Salaries"));
        let (kept,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM settings WHERE key = 'rolled-back'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(kept, 0, "the operation rolled back, its record didn't");

        // The page reports the same failure with the request id: added to the server's record, not a second incident
        let res = send(&app, "/api/client-errors", &cookie, Some(json!({ "kind": "handled", "operation": "rename", "message": "A server error occurred", "request_id": request, "status": 500 }))).await;
        assert_eq!(res.status(), StatusCode::ACCEPTED);
        settle().await;
        let all = rows(&env.st, 1).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].client, "rename: A server error occurred");

        // The same failure again: counted on the record, which now carries the latest request id
        let res = send(&app, "/api/items/0123abcd/fail", &cookie, None).await;
        let again = res.headers()["x-request-id"].to_str().unwrap().to_string();
        settle().await;
        let all = rows(&env.st, 1).await;
        assert_eq!((all.len(), all[0].count), (1, 2));
        assert_eq!(all[0].request_id.as_deref(), Some(again.as_str()));

        // A refusal is a warning, with the names in its message left out
        let res = send(&app, "/api/items/0123abcd/refuse", &cookie, None).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let r = &rows(&env.st, 2).await[0];
        assert_eq!((r.severity.as_str(), r.kind.as_str(), r.message.as_str()), ("warning", "conflict", "An item named \"…\" already exists"));

        // Nobody signed in: recorded without a person
        let res = send(&app, "/api/client-errors", "", Some(json!({ "kind": "uncaught", "message": "boom" }))).await;
        assert_eq!(res.status(), StatusCode::ACCEPTED);
        let r = &rows(&env.st, 3).await[0];
        assert_eq!((r.source.as_str(), r.user_id, r.username.as_str()), ("frontend", None, ""));
    }

    #[tokio::test]
    async fn reports_are_rate_limited_and_size_limited_and_repeats_are_counted() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (_, cookie) = env.sign_in(&amy, "Test").await;
        let app = failing_app(env.st.clone());
        let mut accepted = 0;
        for _ in 0..(CLIENT_PER_MINUTE + 5) {
            let res = send(&app, "/api/client-errors", &cookie, Some(json!({ "kind": "rejection", "message": "loop" }))).await;
            if res.status() == StatusCode::ACCEPTED {
                accepted += 1;
            } else {
                assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
                let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
                assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["recorded"], false, "a dropped report doesn't claim to be recorded");
            }
        }
        // (a new minute may start in between, letting a few more through)
        assert!((CLIENT_PER_MINUTE..=CLIENT_PER_MINUTE * 2).contains(&accepted), "{accepted}");
        settle().await;
        let all = rows(&env.st, 1).await;
        assert_eq!(all.len(), 1, "repeats are one record");
        assert_eq!(all[0].count, accepted as i64);

        let big = json!({ "kind": "render", "message": "x".repeat(MAX_REPORT) });
        assert_eq!(send(&app, "/api/client-errors", "", Some(big)).await.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(send(&app, "/api/client-errors", "", Some(json!({ "kind": "nope" }))).await.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn only_administrators_see_the_error_log_and_it_filters() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (_, cookie) = env.sign_in(&amy, "Test").await;
        let app = failing_app(env.st.clone());
        send(&app, "/api/items/a1/fail", &cookie, None).await;
        send(&app, "/api/client-errors", &cookie, Some(json!({ "kind": "uncaught", "message": "boom" }))).await;
        rows(&env.st, 2).await;
        let q = |f: fn(&mut ErrorQuery)| {
            let mut q = ErrorQuery::default();
            f(&mut q);
            q
        };
        assert_eq!(query_errors(&env.st, &q(|q| q.source = Some("frontend".into())), 10).await.unwrap().len(), 1);
        assert_eq!(query_errors(&env.st, &q(|q| q.severity = Some("warning".into())), 10).await.unwrap().len(), 0);
        assert_eq!(query_errors(&env.st, &q(|q| q.user = Some("am".into())), 10).await.unwrap().len(), 2);
        assert_eq!(query_errors(&env.st, &q(|q| q.q = Some("boom".into())), 10).await.unwrap().len(), 1);

        // Through the real routes: administrators only
        let router = crate::app::routes::router(env.st.clone());
        let get = |cookie: String| {
            let req = axum::http::Request::get("/api/admin/errors").header(axum::http::header::COOKIE, cookie).extension(ConnectInfo(SocketAddr::from(([10, 0, 0, 1], 5000)))).body(Body::empty()).unwrap();
            router.clone().oneshot(req)
        };
        assert_eq!(get(cookie.clone()).await.unwrap().status(), StatusCode::FORBIDDEN);
        let (_, admin_cookie) = env.sign_in(&env.admin().await, "Test").await;
        assert_eq!(get(admin_cookie).await.unwrap().status(), StatusCode::OK);
    }
}
