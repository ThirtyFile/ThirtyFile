//! WebDAV (RFC 4918, classes 1 and 2) at `/dav/`: ThirtyFile mapped as a drive on Windows and macOS, and used from
//! the Files apps of phones and from tools such as rclone.
//!
//! - `/dav/` shows the user's spaces as folders ("My files", the company space, team spaces) and "Shared with me"
//!   (items shared from spaces the user isn't a member of); below each is the folder tree. Names that clash get a
//!   number: "Projects (2)"
//! - Only app passwords sign in (HTTP Basic with the username and an app password, or Bearer), with the same limit on
//!   wrong attempts as the sign-in page. Cookies are ignored and never set. A read-only app password may only use GET,
//!   HEAD, OPTIONS and PROPFIND
//! - Every change goes through the same code as the web: permissions, read-only spaces, folder spaces, quotas, the
//!   upload size limit, name rules, the trash (DELETE) and the activity log
//! - Locks are only acknowledged: LOCK hands out a token and UNLOCK accepts any. That is what Windows Explorer, macOS
//!   Finder and Office need before they save; locks aren't enforced, so the last save wins
//! - PROPFIND answers Depth 0 and 1 (1 when the header is missing); Depth infinity is refused, as RFC 4918 allows
//! - PROPPATCH is answered as if it worked, but nothing is stored (clients use it to set times and Windows attributes)

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use axum::{
    Json,
    body::Body,
    extract::{self, Request, State},
    http::{HeaderMap, Method, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use quick_xml::{NsReader, escape::escape, events::Event, name::ResolveResult};
use serde_json::{Value, json};
use sqlx::SqliteConnection;
use tokio::io::AsyncWriteExt;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    files, fsops, nodes,
    logs,
    paths::{Found, SHARED, Target, child_named, resolve, tops},
    state::AppState,
    tokens,
    tree::{self, Need, Node},
    util::{guess_mime, new_id, now, numbered_name, validate_name},
};

/// Where WebDAV is served
pub const PREFIX: &str = "/dav";
/// Largest XML request body (PROPFIND, PROPPATCH, LOCK)
const MAX_XML: usize = 1024 * 1024;
/// Largest file a PUT may send when no upload size limit is set (sizes are summed as i64 for quotas)
const MAX_PUT: u64 = 1 << 50;
const ALLOW: &str = "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, MKCOL, COPY, MOVE, LOCK, UNLOCK";
/// Characters left as they are in a path segment of an href; everything else is percent-encoded
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');
const DAV_NS: &str = "DAV:";

/// Every request to `/dav` and below
pub async fn handle(State(st): State<AppState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    // Clients ask what the server can do before signing in (Windows does)
    if parts.method == Method::OPTIONS {
        return options();
    }
    let user = match authenticate(&st, &parts).await {
        Ok(user) => user,
        Err(e) if e.status == StatusCode::UNAUTHORIZED => return challenge(),
        Err(e) => return fail(e),
    };
    let Some(segs) = segments(parts.uri.path()) else { return fail(AppError::not_found("Not found")) };
    let h = &parts.headers;
    let result = match parts.method.as_str() {
        "GET" | "HEAD" => get(&st, &user, &segs, h).await,
        "PROPFIND" => propfind(&st, &user, &segs, h, body).await,
        "PROPPATCH" => proppatch(&st, &user, &segs, body).await,
        "PUT" => put(&st, &user, &segs, h, body).await,
        "DELETE" => delete(&st, &user, &segs).await,
        "MKCOL" => mkcol(&st, &user, &segs, body).await,
        "MOVE" => transfer(&st, &user, &segs, h, true).await,
        "COPY" => transfer(&st, &user, &segs, h, false).await,
        "LOCK" => lock(&st, &user, &segs, h, body).await,
        // Locks aren't kept, so any token is accepted
        "UNLOCK" => Ok(StatusCode::NO_CONTENT.into_response()),
        _ => Ok((StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, ALLOW)]).into_response()),
    };
    let mut res = result.unwrap_or_else(fail);
    res.headers_mut().remove(header::SET_COOKIE);
    res
}

/// The site's root: Windows asks it whether the server speaks WebDAV (OPTIONS and PROPFIND /) before it opens `/dav/`
pub async fn server_root(st: State<AppState>, req: Request) -> Response {
    match *req.method() {
        Method::GET | Method::HEAD => crate::web::serve(st, req.uri().clone(), req.headers().clone()).await,
        Method::OPTIONS => options(),
        _ => (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "OPTIONS, GET, HEAD")]).into_response(),
    }
}

fn options() -> Response {
    (StatusCode::OK, [("dav", "1, 2"), ("allow", ALLOW), ("ms-author-via", "DAV"), ("content-length", "0")]).into_response()
}

/// Signs the request in with its app password (a missing or wrong one is answered with a sign-in challenge)
async fn authenticate(st: &AppState, parts: &Parts) -> AppResult<User> {
    let credential = tokens::credential(&parts.headers).ok_or_else(AppError::unauthorized)?;
    tokens::authenticate(parts, st, credential).await
}

fn challenge() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"ThirtyFile\", charset=\"UTF-8\""), (header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "Sign in with your username and an app password\n",
    )
        .into_response()
}

/// Errors as plain text: WebDAV clients show the status, not a JSON body
fn fail(e: AppError) -> Response {
    (e.status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], format!("{}\n", e.message)).into_response()
}

fn not_found() -> AppError {
    AppError::not_found("Not found")
}

fn precondition(m: &str) -> AppError {
    AppError::new(StatusCode::PRECONDITION_FAILED, m)
}

// ───────────── Paths ─────────────

/// The decoded path segments below `/dav`; None for a path outside it or with `.`/`..`
fn segments(path: &str) -> Option<Vec<String>> {
    let rest = path.strip_prefix(PREFIX)?;
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    let mut out = Vec::new();
    for raw in rest.split('/').filter(|s| !s.is_empty()) {
        let seg = percent_decode_str(raw).decode_utf8().ok()?.into_owned();
        if seg == "." || seg == ".." || seg.contains('/') {
            return None;
        }
        out.push(seg);
    }
    Some(out)
}

/// The percent-encoded path of these segments; collections end with a slash
fn href(segs: &[String], collection: bool) -> String {
    let mut s = format!("{PREFIX}/");
    s.push_str(&segs.iter().map(|x| utf8_percent_encode(x, SEGMENT).to_string()).collect::<Vec<_>>().join("/"));
    if collection && !segs.is_empty() {
        s.push('/');
    }
    s
}

/// The Destination of a MOVE or COPY: a full URL or a path
fn destination(headers: &HeaderMap) -> AppResult<Vec<String>> {
    let raw = headers.get("destination").and_then(|v| v.to_str().ok()).ok_or_else(|| AppError::bad_request("Missing Destination"))?;
    let path = match raw.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]),
        None => raw,
    };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    segments(path).ok_or_else(|| AppError::new(StatusCode::BAD_GATEWAY, "The destination isn't on this WebDAV server"))
}

// ───────────── Finding items ─────────────

/// The folder a new item at this path goes into, and the item's name
async fn parent_of(conn: &mut SqliteConnection, user: &User, segs: &[String]) -> AppResult<(Node, String)> {
    let Some((last, parent)) = segs.split_last() else { return Err(AppError::forbidden("Spaces can't be changed here")) };
    let found = resolve(conn, user, parent).await?.ok_or_else(|| AppError::conflict("The folder doesn't exist"))?;
    let folder = match found.target {
        Target::Node(n) if n.is_folder() => *n,
        Target::Node(_) => return Err(AppError::conflict("The folder doesn't exist")),
        Target::Root | Target::Shared => return Err(AppError::forbidden("Items can only be added inside a space or a shared folder")),
    };
    let name = validate_name(last)?;
    if name != *last {
        return Err(AppError::bad_request("Name can't start or end with a space"));
    }
    Ok((folder, name))
}

async fn found_node(st: &AppState, user: &User, segs: &[String]) -> AppResult<Node> {
    match resolve(&mut *st.db.acquire().await?, user, segs).await?.ok_or_else(not_found)?.target {
        Target::Node(n) => Ok(*n),
        Target::Root | Target::Shared => Err(AppError::forbidden("This folder can't be changed")),
    }
}

/// Calls a web handler with a JSON request, so WebDAV changes go through the same checks and logging as the web
fn json_req<T: serde::de::DeserializeOwned>(v: Value) -> AppResult<Json<T>> {
    serde_json::from_value(v).map(Json).map_err(AppError::internal)
}

async fn rename(st: &AppState, user: &User, id: &str, name: &str) -> AppResult<()> {
    nodes::rename(State(st.clone()), user.clone(), extract::Path(id.to_string()), json_req(json!({ "name": name }))?).await.map(|_| ())
}

async fn move_into(st: &AppState, user: &User, id: &str, dest: &str) -> AppResult<()> {
    nodes::move_nodes(State(st.clone()), user.clone(), json_req(json!({ "ids": [id], "dest_id": dest }))?).await.map(|_| ())
}

async fn trash(st: &AppState, user: &User, id: &str) -> AppResult<()> {
    nodes::trash(State(st.clone()), user.clone(), json_req(json!({ "ids": [id] }))?).await.map(|_| ())
}

// ───────────── Properties ─────────────

/// Which properties a PROPFIND asks for
#[derive(Debug, PartialEq)]
enum Want {
    All,
    Names,
    /// (namespace, local name)
    Props(Vec<(String, String)>),
}

impl Want {
    fn asks_for(&self, name: &str) -> bool {
        matches!(self, Want::Props(list) if list.iter().any(|(ns, local)| ns == DAV_NS && local == name))
    }
}

/// What the XML body of a PROPFIND, PROPPATCH or LOCK says
#[derive(Default, Debug)]
struct Parsed {
    allprop: bool,
    propname: bool,
    /// A `prop` element was there (PROPFIND for named properties)
    prop: bool,
    /// Children of every `prop` element: (namespace, local name)
    props: Vec<(String, String)>,
    /// LOCK: the text of `owner`
    owner: String,
    /// LOCK: a shared lock was asked for
    shared: bool,
}

fn parse(xml: &[u8]) -> AppResult<Parsed> {
    let bad = || AppError::bad_request("The XML in the request isn't valid");
    let mut out = Parsed::default();
    if xml.iter().all(u8::is_ascii_whitespace) {
        return Ok(out);
    }
    let mut r = NsReader::from_reader(xml);
    r.config_mut().trim_text(true);
    // The open elements: (namespace, local name)
    let mut open: Vec<(String, String)> = Vec::new();
    loop {
        let (ns, event) = r.read_resolved_event().map_err(|_| bad())?;
        let ns = match ns {
            ResolveResult::Bound(n) => n.as_ref().to_owned(),
            _ => String::new(),
        };
        let (start, empty) = match &event {
            Event::Start(e) => (Some(e), false),
            Event::Empty(e) => (Some(e), true),
            _ => (None, false),
        };
        if let Some(e) = start {
            let local = e.local_name().as_ref().to_owned();
            let in_prop = open.last().is_some_and(|(n, l)| n == DAV_NS && l == "prop");
            let in_owner = open.iter().any(|(n, l)| n == DAV_NS && l == "owner");
            if in_prop {
                out.props.push((ns.clone(), local.clone()));
            } else if ns == DAV_NS && !in_owner {
                match local.as_str() {
                    "allprop" => out.allprop = true,
                    "propname" => out.propname = true,
                    "prop" => out.prop = true,
                    "shared" if open.iter().any(|(n, l)| n == DAV_NS && l == "lockscope") => out.shared = true,
                    _ => {}
                }
            }
            if !empty {
                open.push((ns, local));
            }
            continue;
        }
        match event {
            Event::End(_) => {
                open.pop();
            }
            Event::Text(t) if open.iter().any(|(n, l)| n == DAV_NS && l == "owner") => {
                out.owner.push_str(&t.xml10_content());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

impl Parsed {
    fn want(self) -> Want {
        if self.propname {
            Want::Names
        } else if self.prop && !self.allprop {
            Want::Props(self.props)
        } else {
            Want::All
        }
    }
}

async fn read_xml(body: Body) -> AppResult<Parsed> {
    let bytes = axum::body::to_bytes(body, MAX_XML).await.map_err(|_| AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "The request is too large"))?;
    parse(&bytes)
}

/// Seconds since 1970 in the format of HTTP headers (RFC 1123): "Sun, 06 Nov 1994 08:49:37 GMT"
fn http_date(t: i64) -> String {
    httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(t.max(0) as u64))
}

/// Seconds since 1970 as RFC 3339 in UTC: "1994-11-06T08:49:37Z"
fn rfc3339(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86400), t.rem_euclid(86400));
    // Days to a civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

const SUPPORTED_LOCK: &str = "<D:lockentry><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockentry>\
<D:lockentry><D:lockscope><D:shared/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockentry>";

/// Properties (DAV: namespace) of an item, as XML content; `quota`: (used, available) of the space
fn node_props(node: &Node, display: &str, quota: Option<(i64, i64)>) -> Vec<(&'static str, String)> {
    let mut p = vec![
        ("displayname", escape(display).into_owned()),
        ("resourcetype", if node.is_folder() { "<D:collection/>".into() } else { String::new() }),
        ("getlastmodified", http_date(node.updated_at)),
        ("creationdate", rfc3339(node.created_at)),
    ];
    if node.is_folder() {
        p.push(("getetag", format!("\"{}-{}\"", node.id, node.updated_at)));
    } else {
        p.push(("getcontentlength", node.size.to_string()));
        let mime = if node.mime.is_empty() { "application/octet-stream" } else { &node.mime };
        p.push(("getcontenttype", escape(mime).into_owned()));
        // The same tag GET gives stored content; files of folder spaces get one from their index entry
        let tag = match &node.blob_hash {
            Some(hash) if !node.in_folder_space() => hash.clone(),
            _ => format!("{}-{}-{}", node.id, node.updated_at, node.size),
        };
        p.push(("getetag", format!("\"{}\"", escape(&tag))));
    }
    p.push(("supportedlock", SUPPORTED_LOCK.into()));
    p.push(("lockdiscovery", String::new()));
    if let Some((used, available)) = quota {
        p.push(("quota-used-bytes", used.to_string()));
        p.push(("quota-available-bytes", available.to_string()));
    }
    p
}

/// Properties of `/dav/` and "Shared with me", which aren't items
fn folder_props(display: &str) -> Vec<(&'static str, String)> {
    vec![
        ("displayname", escape(display).into_owned()),
        ("resourcetype", "<D:collection/>".into()),
        ("getlastmodified", http_date(now())),
        ("supportedlock", SUPPORTED_LOCK.into()),
        ("lockdiscovery", String::new()),
    ]
}

/// A 207 Multi-Status response being written
struct Multistatus {
    xml: String,
}

impl Multistatus {
    fn new() -> Self {
        Multistatus { xml: r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">"#.into() }
    }

    fn propstat(&mut self, props: &str, status: &str) {
        self.xml.push_str(&format!("<D:propstat><D:prop>{props}</D:prop><D:status>HTTP/1.1 {status}</D:status></D:propstat>"));
    }

    /// One item with the properties asked for; the ones it doesn't have are reported as not found
    fn add(&mut self, href: &str, want: &Want, props: Vec<(&'static str, String)>) {
        let element = |name: &str, value: &str| if value.is_empty() { format!("<D:{name}/>") } else { format!("<D:{name}>{value}</D:{name}>") };
        let mut found = String::new();
        let mut missing = String::new();
        match want {
            // Quotas are only reported when asked for by name (RFC 4331)
            Want::All => props.iter().filter(|(n, _)| !n.starts_with("quota-")).for_each(|(n, v)| found.push_str(&element(n, v))),
            Want::Names => props.iter().for_each(|(n, _)| found.push_str(&format!("<D:{n}/>"))),
            Want::Props(list) => {
                for (ns, local) in list {
                    match props.iter().find(|(n, _)| ns == DAV_NS && n == local) {
                        Some((n, v)) => found.push_str(&element(n, v)),
                        None => missing.push_str(&foreign(ns, local)),
                    }
                }
            }
        }
        self.xml.push_str(&format!("<D:response><D:href>{}</D:href>", escape(href)));
        if !found.is_empty() || missing.is_empty() {
            self.propstat(&found, "200 OK");
        }
        if !missing.is_empty() {
            self.propstat(&missing, "404 Not Found");
        }
        self.xml.push_str("</D:response>");
    }

    fn finish(mut self) -> Response {
        self.xml.push_str("</D:multistatus>");
        (StatusCode::MULTI_STATUS, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], self.xml).into_response()
    }
}

/// An empty element named as the request named it
fn foreign(ns: &str, local: &str) -> String {
    match ns {
        DAV_NS => format!("<D:{local}/>"),
        "" => format!("<{local} xmlns=\"\"/>"),
        _ => format!("<R:{local} xmlns:R=\"{}\"/>", escape(ns)),
    }
}

/// (used, available) bytes of a space with a quota, remembered per space for one response
async fn quota(conn: &mut SqliteConnection, cache: &mut HashMap<String, Option<(i64, i64)>>, drive_id: &str) -> AppResult<Option<(i64, i64)>> {
    if let Some(q) = cache.get(drive_id) {
        return Ok(*q);
    }
    let q = match tree::get_drive(conn, drive_id).await? {
        Some(d) => {
            let limit = tree::drive_quota(conn, &d).await?;
            (limit > 0).then(|| (d.used_bytes, (limit - d.used_bytes).max(0)))
        }
        None => None,
    };
    cache.insert(drive_id.to_string(), q);
    Ok(q)
}

// ───────────── Methods ─────────────

async fn propfind(st: &AppState, user: &User, segs: &[String], headers: &HeaderMap, body: Body) -> AppResult<Response> {
    let depth = match headers.get("depth").and_then(|v| v.to_str().ok()).map(str::trim) {
        Some("0") => 0,
        Some("1") | None => 1,
        Some(d) if d.eq_ignore_ascii_case("infinity") => {
            let xml = r#"<?xml version="1.0" encoding="utf-8"?><D:error xmlns:D="DAV:"><D:propfind-finite-depth/></D:error>"#;
            return Ok((StatusCode::FORBIDDEN, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], xml).into_response());
        }
        Some(_) => return Err(AppError::bad_request("Depth must be 0 or 1")),
    };
    let want = read_xml(body).await?.want();
    let with_quota = want.asks_for("quota-used-bytes") || want.asks_for("quota-available-bytes");
    let mut quotas = HashMap::new();
    let mut c = st.db.acquire().await?;
    let found = resolve(&mut c, user, segs).await?.ok_or_else(not_found)?;
    let mut out = Multistatus::new();
    let path = found.path;
    let child = |name: &str| {
        let mut p = path.clone();
        p.push(name.to_string());
        p
    };
    match found.target {
        Target::Root => {
            out.add(&href(&[], true), &want, folder_props("ThirtyFile"));
            if depth == 1 {
                for (name, root) in tops(&mut c, user).await?.spaces {
                    let Some(node) = tree::get_node(&mut c, &root).await? else { continue };
                    let q = if with_quota { quota(&mut c, &mut quotas, node.drive()).await? } else { None };
                    out.add(&href(&child(&name), true), &want, node_props(&node, &name, q));
                }
                out.add(&href(&child(SHARED), true), &want, folder_props(SHARED));
            }
        }
        Target::Shared => {
            out.add(&href(&path, true), &want, folder_props(SHARED));
            if depth == 1 {
                for (name, node) in tops(&mut c, user).await?.shared {
                    let q = if with_quota && node.is_folder() { quota(&mut c, &mut quotas, node.drive()).await? } else { None };
                    out.add(&href(&child(&name), node.is_folder()), &want, node_props(&node, &name, q));
                }
            }
        }
        Target::Node(node) => {
            let display = path.last().cloned().unwrap_or_default();
            let q = if with_quota && node.is_folder() { quota(&mut c, &mut quotas, node.drive()).await? } else { None };
            out.add(&href(&path, node.is_folder()), &want, node_props(&node, &display, q));
            if depth == 1 && node.is_folder() {
                if node.in_folder_space() {
                    // Changes made on the server's folder show up when it is opened, as on the web (without holding
                    // a connection meanwhile: syncing takes its own)
                    drop(c);
                    crate::folders::sync_folder(st, &node).await;
                    c = st.db.acquire().await?;
                }
                let mut list = nodes::list_children(&mut c, &node.id, &nodes::ListQuery::default()).await?;
                for n in std::mem::take(list.items_mut()) {
                    let q = if with_quota && n.is_folder() { quota(&mut c, &mut quotas, n.drive()).await? } else { None };
                    out.add(&href(&child(&n.name), n.is_folder()), &want, node_props(&n, &n.name, q));
                }
            }
        }
    }
    Ok(out.finish())
}

/// Accepted and not stored: every property is reported as set
async fn proppatch(st: &AppState, user: &User, segs: &[String], body: Body) -> AppResult<Response> {
    let parsed = read_xml(body).await?;
    let found = resolve(&mut *st.db.acquire().await?, user, segs).await?.ok_or_else(not_found)?;
    let collection = !matches!(&found.target, Target::Node(n) if !n.is_folder());
    let mut out = Multistatus::new();
    let props: String = parsed.props.iter().map(|(ns, local)| foreign(ns, local)).collect();
    out.xml.push_str(&format!("<D:response><D:href>{}</D:href>", escape(href(&found.path, collection))));
    out.propstat(&props, "200 OK");
    out.xml.push_str("</D:response>");
    Ok(out.finish())
}

async fn get(st: &AppState, user: &User, segs: &[String], headers: &HeaderMap) -> AppResult<Response> {
    match resolve(&mut *st.db.acquire().await?, user, segs).await?.ok_or_else(not_found)?.target {
        Target::Node(node) if !node.is_folder() => files::serve_blob(st, headers, files::node_blob(&node)?, false).await,
        _ => Ok((StatusCode::OK, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "This is a WebDAV folder of ThirtyFile. Open it with a WebDAV client, or map it as a network drive.\n").into_response()),
    }
}

async fn delete(st: &AppState, user: &User, segs: &[String]) -> AppResult<Response> {
    let node = found_node(st, user, segs).await?;
    if node.parent_id.is_none() {
        return Err(AppError::forbidden("A space can't be deleted here"));
    }
    trash(st, user, &node.id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn mkcol(st: &AppState, user: &User, segs: &[String], body: Body) -> AppResult<Response> {
    let bytes = axum::body::to_bytes(body, MAX_XML).await.map_err(|_| AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "The request is too large"))?;
    if !bytes.is_empty() {
        return Err(AppError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "MKCOL doesn't take a body"));
    }
    let (parent, name) = {
        let mut c = st.db.acquire().await?;
        let (parent, name) = parent_of(&mut c, user, segs).await?;
        if child_named(&mut c, &parent.id, &name).await?.is_some() {
            return Err(AppError::new(StatusCode::METHOD_NOT_ALLOWED, "An item with this name already exists"));
        }
        (parent, name)
    };
    let _ = nodes::create_folder(State(st.clone()), user.clone(), json_req(json!({ "parent_id": parent.id, "name": name }))?).await?;
    Ok(StatusCode::CREATED.into_response())
}

/// MOVE and COPY, with Destination and Overwrite (an item already at the destination goes to the trash)
async fn transfer(st: &AppState, user: &User, segs: &[String], headers: &HeaderMap, moving: bool) -> AppResult<Response> {
    let dest_segs = destination(headers)?;
    let overwrite = !headers.get("overwrite").is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"F"));
    let src = found_node(st, user, segs).await?;
    if src.parent_id.is_none() {
        return Err(AppError::forbidden("A space can't be moved or copied here"));
    }
    let mut c = st.db.acquire().await?;
    let (dest, name) = parent_of(&mut c, user, &dest_segs).await?;
    if tree::is_within(&mut c, &dest.id, &src.id).await? {
        return Err(AppError::forbidden("A folder can't go into itself"));
    }
    let mut replaced = false;
    match child_named(&mut c, &dest.id, &name).await? {
        Some(existing) if existing.id == src.id => {
            if !moving {
                return Err(AppError::forbidden("The source and destination are the same"));
            }
        }
        Some(existing) => {
            if !overwrite {
                return Err(precondition("The destination already exists"));
            }
            drop(c);
            trash(st, user, &existing.id).await?;
            replaced = true;
        }
        None => drop(c),
    }
    if moving {
        move_to(st, user, &src, &dest, &name).await?;
    } else {
        copy_to(st, user, &src, &dest, &name).await?;
    }
    Ok(if replaced { StatusCode::NO_CONTENT } else { StatusCode::CREATED }.into_response())
}

/// Moves an item into `dest` as `name`: a rename, a move, or both (in the order the names allow)
async fn move_to(st: &AppState, user: &User, src: &Node, dest: &Node, name: &str) -> AppResult<()> {
    let from = src.parent_id.clone().unwrap_or_default();
    if from == dest.id {
        return rename(st, user, &src.id, name).await;
    }
    if name == src.name {
        return move_into(st, user, &src.id, &dest.id).await;
    }
    let mut c = st.db.acquire().await?;
    if !tree::name_taken(&mut c, &dest.id, &src.name).await? {
        drop(c);
        move_into(st, user, &src.id, &dest.id).await?;
        return rename(st, user, &src.id, name).await;
    }
    if !tree::name_taken(&mut c, &from, name).await? {
        drop(c);
        rename(st, user, &src.id, name).await?;
        return move_into(st, user, &src.id, &dest.id).await;
    }
    // Both names are taken on the other side: pass through a name free in both folders
    let mut via = None;
    for n in 1..10_000 {
        let candidate = numbered_name(name, n, src.is_folder());
        if !tree::name_taken(&mut c, &dest.id, &candidate).await? && !tree::name_taken(&mut c, &from, &candidate).await? {
            via = Some(candidate);
            break;
        }
    }
    drop(c);
    let via = via.ok_or_else(|| AppError::conflict("Too many items with the same name"))?;
    rename(st, user, &src.id, &via).await?;
    move_into(st, user, &src.id, &dest.id).await?;
    rename(st, user, &src.id, name).await
}

/// (id, name) of a folder's items of one kind, newest first
async fn children_of(conn: &mut SqliteConnection, parent_id: &str, kind: &str) -> AppResult<Vec<(String, String)>> {
    Ok(sqlx::query_as("SELECT id, name FROM nodes WHERE parent_id = ? AND kind = ? AND trashed_at IS NULL ORDER BY created_at DESC")
        .bind(parent_id)
        .bind(kind)
        .fetch_all(conn)
        .await?)
}

/// Copies an item into `dest` as `name` (the copy gets a free name first, then the one asked for)
async fn copy_to(st: &AppState, user: &User, src: &Node, dest: &Node, name: &str) -> AppResult<()> {
    let before: HashSet<String> = children_of(&mut *st.db.acquire().await?, &dest.id, &src.kind).await?.into_iter().map(|(id, _)| id).collect();
    let _ = nodes::copy_nodes(State(st.clone()), user.clone(), json_req(json!({ "ids": [src.id], "dest_id": dest.id }))?).await?;
    let after = children_of(&mut *st.db.acquire().await?, &dest.id, &src.kind).await?;
    let (id, copied) = after.into_iter().find(|(id, _)| !before.contains(id)).ok_or_else(|| AppError::internal("the copy wasn't found"))?;
    if copied != name {
        rename(st, user, &id, name).await?;
    }
    Ok(())
}

fn check_size(st: &AppState, size: u64) -> AppResult<()> {
    if size > MAX_PUT || (st.max_upload > 0 && size > st.max_upload) {
        return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "The file exceeds the upload size limit"));
    }
    Ok(())
}

/// Creates or replaces a file. The body is received into a temporary file first, then stored like an upload
async fn put(st: &AppState, user: &User, segs: &[String], headers: &HeaderMap, body: Body) -> AppResult<Response> {
    let (parent, name) = {
        let mut c = st.db.acquire().await?;
        let (parent, name) = parent_of(&mut c, user, segs).await?;
        // Checked before anything is received: nobody without write access makes the server take a file
        let parent = tree::folder_for(&mut c, user, &parent.id, Need::Write).await?;
        (parent, name)
    };
    let mut c = st.db.acquire().await?;
    let mut existing = child_named(&mut c, &parent.id, &name).await?;
    // Only when the index and the folder on the server disagree about this name is the folder looked at again:
    // copying thousands of files into one folder mustn't re-read it for each
    if parent.in_folder_space() {
        let on_disk = parent.fs_pinned().and_then(|p| p.join(&name)).is_ok_and(|p| std::fs::symlink_metadata(p.as_path()).is_ok());
        if on_disk != existing.is_some() {
            drop(c);
            crate::folders::sync_folder(st, &parent).await;
            c = st.db.acquire().await?;
            existing = child_named(&mut c, &parent.id, &name).await?;
        }
    }
    if let Some(n) = &existing {
        if n.is_folder() {
            return Err(AppError::new(StatusCode::METHOD_NOT_ALLOWED, "A folder has this name"));
        }
        tree::node_for(&mut c, user, &n.id, Need::Write).await?;
        if headers.get(header::IF_NONE_MATCH).is_some_and(|v| v.as_bytes() == b"*") {
            return Err(precondition("The file already exists"));
        }
    }
    if let Some(len) = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok()) {
        check_size(st, len)?;
        tree::check_quota(&mut c, parent.drive(), len as i64 - existing.as_ref().map_or(0, |n| n.size)).await?;
    }
    drop(c);
    let tmp = st.tmp_dir().join(format!("dav-{}", new_id()));
    // Content for the content store is hashed as it arrives, so storing it doesn't read the file again
    let (size, hash) = match receive(st, body, &tmp, !parent.in_folder_space()).await {
        Ok(received) => received,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
    };
    // Stored in a task of its own, so a dropped request can't stop it halfway
    let created = tokio::spawn(store(st.clone(), user.clone(), parent, name, tmp, size, hash)).await.map_err(AppError::internal)??;
    Ok(if created { StatusCode::CREATED } else { StatusCode::NO_CONTENT }.into_response())
}

/// Writes the body to a file, within the upload size limit; returns its size, and with `hash` its SHA-256
async fn receive(st: &AppState, body: Body, path: &Path, hash: bool) -> AppResult<(u64, Option<String>)> {
    use sha2::Digest;
    let mut file = tokio::fs::File::create(path).await?;
    let mut stream = body.into_data_stream();
    let mut size = 0u64;
    let mut hasher = hash.then(sha2::Sha256::new);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| AppError::bad_request("Connection interrupted"))?;
        size += chunk.len() as u64;
        check_size(st, size)?;
        file.write_all(&chunk).await?;
        if let Some(h) = &mut hasher {
            h.update(&chunk);
        }
    }
    file.flush().await?;
    file.sync_data().await?;
    Ok((size, hasher.map(|h| hex::encode(h.finalize()))))
}

/// Stores a received file as `name` in `parent`, replacing a file of that name; true when it was created. `hash`: the
/// content's SHA-256 when it was computed while receiving it
async fn store(st: AppState, user: User, parent: Node, name: String, tmp: PathBuf, size: u64, hash: Option<String>) -> AppResult<bool> {
    let result = if parent.in_folder_space() {
        store_in_folder(&st, &user, &parent, &name, &tmp, size).await
    } else {
        store_content(&st, &user, &parent, &name, &tmp, size, hash).await
    };
    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

async fn store_content(st: &AppState, user: &User, parent: &Node, name: &str, tmp: &Path, size: u64, hash: Option<String>) -> AppResult<bool> {
    let hash = match hash {
        Some(h) => h,
        None => {
            let (hash, hashed) = files::hash_file(tmp.to_path_buf()).await?;
            if hashed != size {
                return Err(AppError::bad_request("File size mismatch"));
            }
            hash
        }
    };
    let size = size as i64;
    // First into the space's storage location, without holding the write lock (S3 may take a while)
    let staged = tree::stage_blob(st, parent.drive(), hash.clone(), size, tmp.to_path_buf()).await?;
    let _w = st.write_lock.lock().await;
    let result = async {
        let mut tx = st.db.begin().await?;
        // Looked at again under the write lock: the folder or the file may have changed meanwhile
        let folder = tree::folder_for(&mut tx, user, &parent.id, Need::Write).await?;
        if folder.in_folder_space() || folder.drive() != parent.drive() {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        let (created, extra, removed) = match child_named(&mut tx, &folder.id, name).await? {
            Some(n) if n.is_folder() => return Err(AppError::new(StatusCode::METHOD_NOT_ALLOWED, "A folder has this name")),
            Some(n) => {
                let n = tree::node_for(&mut tx, user, &n.id, Need::Write).await?;
                // The same content again (clients often save a file twice): nothing changes
                if n.blob_hash.as_deref() == Some(hash.as_str()) {
                    return Ok((false, None, crate::versions::Removed::default()));
                }
                tree::check_quota(&mut tx, n.drive(), size - n.size).await?;
                let extra = tree::commit_blob(&mut tx, &staged).await?;
                // The content it had is kept as an earlier version
                let removed = tree::set_content(&mut tx, crate::versions::Policy::of(st), &n, &hash, size, user.id).await?;
                tree::touch(&mut tx, &folder.id).await?;
                logs::record_activity(&mut tx, user, Some(&n), "edit", "").await?;
                (false, extra, removed)
            }
            None => {
                tree::check_quota(&mut tx, folder.drive(), size).await?;
                let extra = tree::commit_blob(&mut tx, &staged).await?;
                let id = new_id();
                sqlx::query(
                    "INSERT INTO nodes (id, owner_id, parent_id, kind, name, blob_hash, size, mime, drive_id, created_at, updated_at)
                     SELECT ?1, ?2, ?3, 'file', ?4, ?5, ?6, ?7, drive_id, ?8, ?8 FROM nodes WHERE id = ?3",
                )
                .bind(&id)
                .bind(user.id)
                .bind(&folder.id)
                .bind(name)
                .bind(&hash)
                .bind(size)
                .bind(guess_mime(name))
                .bind(now())
                .execute(&mut *tx)
                .await?;
                tree::touch(&mut tx, &folder.id).await?;
                tree::adjust_usage(&mut tx, folder.drive(), size).await?;
                let node = tree::get_node(&mut tx, &id).await?;
                logs::record_activity(&mut tx, user, node.as_ref(), "upload", "").await?;
                (true, extra, crate::versions::Removed::default())
            }
        };
        tx.commit().await?;
        Ok((created, extra, removed))
    }
    .await;
    match result {
        Ok((created, extra, removed)) => {
            tree::finish_staged(st, staged, extra).await;
            removed.finish(st);
            Ok(created)
        }
        Err(e) => {
            tree::abandon_staged(st, staged).await;
            Err(e)
        }
    }
}

async fn store_in_folder(st: &AppState, user: &User, parent: &Node, name: &str, tmp: &Path, size: u64) -> AppResult<bool> {
    let staged = fsops::stage_upload(parent, tmp, size).await?;
    let _space = fsops::lock_space(parent.drive()).await;
    let _w = st.write_lock.lock().await;
    let result = async {
        let mut tx = st.db.begin().await?;
        let folder = tree::folder_for(&mut tx, user, &parent.id, Need::Write).await?;
        if folder.drive() != parent.drive() {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        let mut removed = crate::versions::Removed::default();
        let created = match child_named(&mut tx, &folder.id, name).await? {
            Some(n) if n.is_folder() => return Err(AppError::new(StatusCode::METHOD_NOT_ALLOWED, "A folder has this name")),
            Some(n) => {
                let n = tree::node_for(&mut tx, user, &n.id, Need::Write).await?;
                tree::check_quota(&mut tx, n.drive(), size as i64 - n.size).await?;
                // The content it had is kept as an earlier version
                removed = fsops::replace_file(&mut tx, crate::versions::Policy::of(st), &staged, &n, user.id).await?;
                let new_size = tree::get_node(&mut tx, &n.id).await?.map_or(n.size, |x| x.size);
                tree::adjust_usage(&mut tx, n.drive(), new_size - n.size).await?;
                logs::record_activity(&mut tx, user, Some(&n), "edit", "").await?;
                false
            }
            None => {
                tree::check_quota(&mut tx, folder.drive(), size as i64).await?;
                let id = fsops::place_file(&mut tx, &staged, user.id, &folder, name).await?;
                tree::touch(&mut tx, &folder.id).await?;
                if let Some(n) = tree::get_node(&mut tx, &id).await? {
                    tree::adjust_usage(&mut tx, n.drive(), n.size).await?;
                    logs::record_activity(&mut tx, user, Some(&n), "upload", "").await?;
                }
                true
            }
        };
        tx.commit().await?;
        removed.finish(st);
        Ok(created)
    }
    .await;
    if result.is_err() {
        // Still under its temporary name unless it was put in place and only the index failed (the next scan shows it then)
        let _ = tokio::fs::remove_file(&staged).await;
    }
    result
}

/// Hands out a lock (see the module notes: not enforced). A path that doesn't exist yet becomes an empty file, as
/// RFC 4918 asks; a refresh (no body, the token in `If`) gets the same token back
async fn lock(st: &AppState, user: &User, segs: &[String], headers: &HeaderMap, body: Body) -> AppResult<Response> {
    let parsed = read_xml(body).await?;
    let refresh = headers.get("if").and_then(|v| v.to_str().ok()).and_then(|v| {
        let start = v.find("<opaquelocktoken:")? + 1;
        let end = start + v[start..].find('>')?;
        Some(v[start..end].to_string())
    });
    let found = resolve(&mut *st.db.acquire().await?, user, segs).await?;
    let (status, path, folder) = match found {
        Some(Found { target: Target::Node(node), path }) => {
            tree::node_for(&mut *st.db.acquire().await?, user, &node.id, Need::Write).await?;
            (StatusCode::OK, path, node.is_folder())
        }
        Some(_) => return Err(AppError::forbidden("This folder can't be locked")),
        None => {
            let (parent, name) = parent_of(&mut *st.db.acquire().await?, user, segs).await?;
            let parent = tree::folder_for(&mut *st.db.acquire().await?, user, &parent.id, Need::Write).await?;
            let tmp = st.tmp_dir().join(format!("dav-{}", new_id()));
            tokio::fs::write(&tmp, b"").await?;
            let mut path = segs[..segs.len() - 1].to_vec();
            path.push(name.clone());
            tokio::spawn(store(st.clone(), user.clone(), parent, name, tmp, 0, None)).await.map_err(AppError::internal)??;
            (StatusCode::CREATED, path, false)
        }
    };
    let token = refresh.unwrap_or_else(|| format!("opaquelocktoken:{}", uuid::Uuid::new_v4()));
    let timeout = headers
        .get("timeout")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').find_map(|t| t.trim().strip_prefix("Second-")?.parse::<u64>().ok()))
        .unwrap_or(3600)
        .min(86400);
    let depth = if folder && headers.get("depth").and_then(|v| v.to_str().ok()) != Some("0") { "infinity" } else { "0" };
    let owner = parsed.owner.trim();
    let owner = if owner.is_empty() { String::new() } else { format!("<D:owner><D:href>{}</D:href></D:owner>", escape(owner)) };
    let scope = if parsed.shared { "<D:shared/>" } else { "<D:exclusive/>" };
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><D:prop xmlns:D="DAV:"><D:lockdiscovery><D:activelock><D:locktype><D:write/></D:locktype><D:lockscope>{scope}</D:lockscope><D:depth>{depth}</D:depth>{owner}<D:timeout>Second-{timeout}</D:timeout><D:locktoken><D:href>{token}</D:href></D:locktoken><D:lockroot><D:href>{}</D:href></D:lockroot></D:activelock></D:lockdiscovery></D:prop>"#,
        escape(href(&path, folder)),
        token = escape(&token),
    );
    let mut res = (status, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], xml).into_response();
    if let Ok(v) = format!("<{token}>").parse::<axum::http::HeaderValue>() {
        res.headers_mut().insert("lock-token", v);
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, TestEnv};
    use axum::{Router, extract::ConnectInfo};
    use tower::ServiceExt;

    async fn app_password(env: &TestEnv, user: &User, scope: &str) -> String {
        let req = serde_json::from_value(json!({ "name": "Drive", "scope": scope, "password": crate::testutil::password() })).unwrap();
        let addr = ConnectInfo(std::net::SocketAddr::from(([10, 0, 0, 9], 5000)));
        let Json(v) = tokens::create(State(env.st.clone()), user.clone(), addr, HeaderMap::new(), Json(req)).await.unwrap();
        v["token"].as_str().unwrap().to_string()
    }

    struct Client {
        app: Router,
        auth: String,
    }

    struct Reply {
        status: StatusCode,
        headers: HeaderMap,
        body: String,
    }

    impl Client {
        async fn new(env: &TestEnv, user: &User, scope: &str) -> Client {
            let token = app_password(env, user, scope).await;
            let auth = format!("Basic {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("{}:{token}", user.username)));
            Client { app: crate::router(env.st.clone()), auth }
        }

        async fn send(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Reply {
            let mut req = axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, &self.auth)
                .extension(ConnectInfo(std::net::SocketAddr::from(([10, 0, 0, 9], 5000))));
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            let res = self.app.clone().oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap();
            let (parts, body) = res.into_parts();
            let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
            Reply { status: parts.status, headers: parts.headers, body: String::from_utf8_lossy(&bytes).into_owned() }
        }

        async fn propfind(&self, path: &str, depth: &str) -> Reply {
            self.send("PROPFIND", path, &[("depth", depth)], "").await
        }
    }

    async fn activity(env: &TestEnv, user: &User) -> Vec<String> {
        sqlx::query_as::<_, (String,)>("SELECT action FROM activity WHERE user_id = ? ORDER BY id").bind(user.id).fetch_all(&env.st.db).await.unwrap().into_iter().map(|(a,)| a).collect()
    }

    #[tokio::test]
    async fn propfind_lists_spaces_shared_items_and_folders() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let amy = env.user("amy", true).await;
        let bob = env.user("bob", true).await;
        env.folder(&amy, &amy.root_id, "Docs").await;
        env.file(&amy, &amy.root_id, "notes.txt").await;
        // A team space whose name clashes with "My files", and a folder Bob shares with Amy
        let req = serde_json::from_value(json!({ "name": "My files" })).unwrap();
        let Json(team) = crate::drives::create(State(env.st.clone()), admin, Json(req)).await.unwrap();
        let team = serde_json::to_value(&team).unwrap();
        env.grant(team["root_id"].as_str().unwrap(), &amy, "editor").await;
        let plans = env.folder(&bob, &bob.root_id, "Plans & ideas").await;
        env.grant(&plans, &amy, "viewer").await;

        let dav = Client::new(&env, &amy, "read").await;
        let res = dav.propfind("/dav/", "1").await;
        assert_eq!(res.status, StatusCode::MULTI_STATUS);
        assert!(res.headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("application/xml"));
        for href in ["<D:href>/dav/</D:href>", "<D:href>/dav/My%20files/</D:href>", "<D:href>/dav/My%20files%20%282%29/</D:href>", "<D:href>/dav/All%20files/</D:href>", "<D:href>/dav/Shared%20with%20me/</D:href>"] {
            assert!(res.body.contains(href), "{href} in {}", res.body);
        }
        assert!(res.body.contains("<D:displayname>My files (2)</D:displayname>"));

        let res = dav.propfind("/dav/My%20files", "1").await;
        assert_eq!(res.status, StatusCode::MULTI_STATUS);
        assert!(res.body.contains("<D:href>/dav/My%20files/Docs/</D:href>"));
        assert!(res.body.contains("<D:href>/dav/My%20files/notes.txt</D:href>"));
        assert!(res.body.contains("<D:resourcetype><D:collection/></D:resourcetype>"));
        assert!(res.body.contains("<D:getcontentlength>0</D:getcontentlength>"));
        assert!(res.body.contains(" GMT</D:getlastmodified>"));
        // Depth 0: the folder alone
        let res = dav.propfind("/dav/my%20FILES/", "0").await;
        assert!(res.body.contains("/dav/My%20files/</D:href>") && !res.body.contains("Docs"));
        assert_eq!(dav.propfind("/dav/My%20files/", "infinity").await.status, StatusCode::FORBIDDEN);

        let res = dav.propfind("/dav/Shared%20with%20me/", "1").await;
        assert!(res.body.contains("<D:href>/dav/Shared%20with%20me/Plans%20%26%20ideas/</D:href>"), "{}", res.body);
        assert!(res.body.contains("<D:displayname>Plans &amp; ideas</D:displayname>"));
        assert_eq!(dav.propfind("/dav/Shared%20with%20me/Plans%20%26%20ideas/", "1").await.status, StatusCode::MULTI_STATUS);

        // Named properties: the ones there are, and the unknown ones as not found
        let body = r#"<?xml version="1.0"?><propfind xmlns="DAV:" xmlns:W="urn:schemas-microsoft-com:"><prop><getcontentlength/><W:Win32FileAttributes/></prop></propfind>"#;
        let res = dav.send("PROPFIND", "/dav/My%20files/notes.txt", &[("depth", "0")], body).await;
        assert!(res.body.contains("<D:getcontentlength>0</D:getcontentlength>") && !res.body.contains("displayname"), "{}", res.body);
        assert!(res.body.contains(r#"<R:Win32FileAttributes xmlns:R="urn:schemas-microsoft-com:"/></D:prop><D:status>HTTP/1.1 404 Not Found"#));
    }

    #[tokio::test]
    async fn only_what_the_user_can_reach_is_found() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let bob = env.user("bob", true).await;
        let secret = env.folder(&bob, &bob.root_id, "Secret").await;
        let bobs = Client::new(&env, &bob, "write").await;
        assert_eq!(bobs.send("PUT", "/dav/My%20files/Secret/plan.txt", &[], "plan").await.status, StatusCode::CREATED);
        let dav = Client::new(&env, &amy, "write").await;
        assert!(!dav.propfind("/dav/", "1").await.body.contains("Secret"));
        for path in ["/dav/Secret/", "/dav/My%20files/Secret/", "/dav/Shared%20with%20me/Secret/", "/dav/Shared%20with%20me/Secret/plan.txt", "/dav/My%20files/../x"] {
            assert_eq!(dav.propfind(path, "0").await.status, StatusCode::NOT_FOUND, "{path}");
        }
        assert_eq!(dav.send("GET", "/dav/Shared%20with%20me/Secret/plan.txt", &[], "").await.status, StatusCode::NOT_FOUND);
        assert_eq!(dav.send("PUT", "/dav/Shared%20with%20me/Secret/new.txt", &[], "x").await.status, StatusCode::CONFLICT);
        // Once shared, it's there
        env.grant(&secret, &amy, "viewer").await;
        assert_eq!(dav.send("GET", "/dav/Shared%20with%20me/Secret/plan.txt", &[], "").await.status, StatusCode::OK);
        // Viewers can't change it
        assert_eq!(dav.send("PUT", "/dav/Shared%20with%20me/Secret/new.txt", &[], "x").await.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn put_and_get_round_trip() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let dav = Client::new(&env, &amy, "write").await;
        let res = dav.send("PUT", "/dav/My%20files/hello%20world.txt", &[], "Hello, WebDAV!").await;
        assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
        let res = dav.send("GET", "/dav/My%20files/hello%20world.txt", &[], "").await;
        assert_eq!((res.status, res.body.as_str()), (StatusCode::OK, "Hello, WebDAV!"));
        let res = dav.send("GET", "/dav/My%20files/hello%20world.txt", &[("range", "bytes=7-12")], "").await;
        assert_eq!((res.status, res.body.as_str()), (StatusCode::PARTIAL_CONTENT, "WebDAV"));
        assert!(!res.headers.contains_key(header::SET_COOKIE));

        // Replacing keeps the file and its id
        let (id,): (String,) = sqlx::query_as("SELECT id FROM nodes WHERE name = 'hello world.txt'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(dav.send("PUT", "/dav/My%20files/hello%20world.txt", &[], "Changed").await.status, StatusCode::NO_CONTENT);
        assert_eq!(dav.send("GET", "/dav/My%20files/hello%20world.txt", &[], "").await.body, "Changed");
        let (again, size): (String, i64) = sqlx::query_as("SELECT id, size FROM nodes WHERE name = 'hello world.txt'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!((again, size), (id, 7));
        assert_eq!(tree::used_bytes(&env.st.db, amy.id).await.unwrap(), 7);
        // The same content again changes nothing
        assert_eq!(dav.send("PUT", "/dav/My%20files/hello%20world.txt", &[], "Changed").await.status, StatusCode::NO_CONTENT);
        assert_eq!(activity(&env, &amy).await, ["upload", "edit"]);

        // Name rules, quota and the upload size limit
        assert_eq!(dav.send("PUT", "/dav/My%20files/a%3Ab.txt", &[], "x").await.status, StatusCode::BAD_REQUEST);
        assert_eq!(dav.send("PUT", "/dav/My%20files/Missing/a.txt", &[], "x").await.status, StatusCode::CONFLICT);
        assert_eq!(dav.send("PUT", "/dav/My%20files", &[], "x").await.status, StatusCode::FORBIDDEN);
        sqlx::query("UPDATE users SET quota_bytes = 10 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        assert_eq!(dav.send("PUT", "/dav/My%20files/big.txt", &[], "0123456789").await.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(dav.send("PUT", "/dav/My%20files/small.txt", &[], "012").await.status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn mkcol_creates_folders() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let dav = Client::new(&env, &amy, "write").await;
        assert_eq!(dav.send("MKCOL", "/dav/My%20files/Projects", &[], "").await.status, StatusCode::CREATED);
        assert_eq!(dav.send("MKCOL", "/dav/My%20files/projects/", &[], "").await.status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(dav.send("MKCOL", "/dav/My%20files/Projects/2026/", &[], "").await.status, StatusCode::CREATED);
        assert_eq!(dav.send("MKCOL", "/dav/My%20files/Nope/2026/", &[], "").await.status, StatusCode::CONFLICT);
        assert_eq!(dav.send("MKCOL", "/dav/New%20space/", &[], "").await.status, StatusCode::FORBIDDEN);
        assert!(dav.propfind("/dav/My%20files/Projects/", "1").await.body.contains("<D:href>/dav/My%20files/Projects/2026/</D:href>"));
        assert_eq!(activity(&env, &amy).await, ["create_folder", "create_folder"]);
    }

    #[tokio::test]
    async fn move_and_copy_with_overwrite() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let dav = Client::new(&env, &amy, "write").await;
        for (path, body) in [("/dav/My%20files/a.txt", "A"), ("/dav/My%20files/b.txt", "B")] {
            assert_eq!(dav.send("PUT", path, &[], body).await.status, StatusCode::CREATED);
        }
        dav.send("MKCOL", "/dav/My%20files/Sub", &[], "").await;
        let to = |dest: &str, overwrite: &str| [("destination", format!("http://localhost{dest}")), ("overwrite", overwrite.to_string())];
        let send = async |method: &str, path: &str, headers: [(&str, String); 2]| {
            let h: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            dav.send(method, path, &h, "").await.status
        };
        assert_eq!(send("MOVE", "/dav/My%20files/a.txt", to("/dav/My%20files/b.txt", "F")).await, StatusCode::PRECONDITION_FAILED);
        assert_eq!(send("MOVE", "/dav/My%20files/a.txt", to("/dav/My%20files/b.txt", "T")).await, StatusCode::NO_CONTENT);
        assert_eq!(dav.send("GET", "/dav/My%20files/b.txt", &[], "").await.body, "A");
        assert_eq!(dav.send("GET", "/dav/My%20files/a.txt", &[], "").await.status, StatusCode::NOT_FOUND);
        // The file that was replaced is in the trash
        let (trashed,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE name = 'b.txt' AND trashed_at IS NOT NULL").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(trashed, 1);

        // Into another folder under a new name; a copy under a new name; a rename that only changes letter case
        assert_eq!(send("MOVE", "/dav/My%20files/b.txt", to("/dav/My%20files/Sub/c.txt", "T")).await, StatusCode::CREATED);
        assert_eq!(send("COPY", "/dav/My%20files/Sub/c.txt", to("/dav/My%20files/Sub/d.txt", "F")).await, StatusCode::CREATED);
        assert_eq!(send("COPY", "/dav/My%20files/Sub/c.txt", to("/dav/My%20files/Sub/C.txt", "F")).await, StatusCode::FORBIDDEN);
        assert_eq!(send("MOVE", "/dav/My%20files/Sub/c.txt", to("/dav/My%20files/Sub/C.txt", "F")).await, StatusCode::CREATED);
        assert_eq!(dav.send("GET", "/dav/My%20files/Sub/d.txt", &[], "").await.body, "A");
        let listing = dav.propfind("/dav/My%20files/Sub/", "1").await.body;
        assert!(listing.contains("/Sub/C.txt<") && listing.contains("/Sub/d.txt<"), "{listing}");
        assert_eq!(send("MOVE", "/dav/My%20files/Sub", to("/dav/My%20files/Sub/Inner", "T")).await, StatusCode::FORBIDDEN);
        assert_eq!(send("MOVE", "/dav/My%20files/Sub", to("https://elsewhere.example/x/Sub", "T")).await, StatusCode::BAD_GATEWAY);
        let actions = activity(&env, &amy).await;
        assert!(actions.contains(&"move".to_string()) && actions.contains(&"copy".to_string()) && actions.contains(&"rename".to_string()), "{actions:?}");
    }

    #[tokio::test]
    async fn delete_moves_to_the_trash() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let dav = Client::new(&env, &amy, "write").await;
        dav.send("MKCOL", "/dav/My%20files/Old", &[], "").await;
        dav.send("PUT", "/dav/My%20files/Old/x.txt", &[], "x").await;
        assert_eq!(dav.send("DELETE", "/dav/My%20files/Old/", &[], "").await.status, StatusCode::NO_CONTENT);
        assert_eq!(dav.propfind("/dav/My%20files/Old/", "0").await.status, StatusCode::NOT_FOUND);
        let (trashed,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE trashed_at IS NOT NULL AND name IN ('Old', 'x.txt')").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(trashed, 2);
        assert!(activity(&env, &amy).await.contains(&"trash".to_string()));
        assert_eq!(dav.send("DELETE", "/dav/My%20files/", &[], "").await.status, StatusCode::FORBIDDEN);
        assert_eq!(dav.send("DELETE", "/dav/", &[], "").await.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn read_only_app_passwords_only_read() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        env.file(&amy, &amy.root_id, "a.txt").await;
        let dav = Client::new(&env, &amy, "read").await;
        assert_eq!(dav.propfind("/dav/My%20files/", "1").await.status, StatusCode::MULTI_STATUS);
        assert_eq!(dav.send("OPTIONS", "/dav/", &[], "").await.status, StatusCode::OK);
        let dest = [("destination", "/dav/My%20files/b.txt")];
        type Case<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);
        let cases: [Case; 7] = [
            ("PUT", "/dav/My%20files/new.txt", &[]),
            ("MKCOL", "/dav/My%20files/New", &[]),
            ("DELETE", "/dav/My%20files/a.txt", &[]),
            ("MOVE", "/dav/My%20files/a.txt", &dest),
            ("COPY", "/dav/My%20files/a.txt", &dest),
            ("PROPPATCH", "/dav/My%20files/a.txt", &[]),
            ("LOCK", "/dav/My%20files/a.txt", &[]),
        ];
        for (method, path, headers) in cases {
            assert_eq!(dav.send(method, path, headers, "").await.status, StatusCode::FORBIDDEN, "{method}");
        }
        assert!(activity(&env, &amy).await.is_empty());
    }

    #[tokio::test]
    async fn only_app_passwords_sign_in() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let app = crate::router(env.st.clone());
        let (_, cookie) = env.sign_in(&amy, "Test").await;
        let send = async |headers: &[(header::HeaderName, String)]| {
            let mut req = axum::http::Request::builder().method("PROPFIND").uri("/dav/").extension(ConnectInfo(std::net::SocketAddr::from(([10, 0, 0, 9], 5000))));
            for (k, v) in headers {
                req = req.header(k, v);
            }
            app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap()
        };
        // A browser session isn't enough, and neither is the account's own password
        for headers in [
            vec![],
            vec![(header::COOKIE, cookie)],
            vec![(header::AUTHORIZATION, format!("Basic {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("amy:{}", testutil::password()))))],
        ] {
            let res = send(&headers).await;
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(res.headers()[header::WWW_AUTHENTICATE], "Basic realm=\"ThirtyFile\", charset=\"UTF-8\"");
        }
        // A page on another site can't use credentials the browser remembers
        let dav = Client::new(&env, &amy, "write").await;
        let res = dav.send("PUT", "/dav/My%20files/x.txt", &[("origin", "https://elsewhere.example")], "x").await;
        assert_eq!(res.status, StatusCode::FORBIDDEN);
        let res = dav.send("OPTIONS", "/dav/", &[], "").await;
        assert_eq!(res.headers["dav"], "1, 2");
        assert!(res.headers["allow"].to_str().unwrap().contains("PROPFIND"));
        // Windows asks the site's root first
        assert_eq!(dav.send("OPTIONS", "/", &[], "").await.headers["dav"], "1, 2");
        assert_eq!(dav.send("PROPFIND", "/", &[("depth", "0")], "").await.status, StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn locks_and_property_changes_are_acknowledged() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let dav = Client::new(&env, &amy, "write").await;
        let body = r#"<?xml version="1.0" encoding="utf-8"?><D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype><D:owner><D:href>Amy</D:href></D:owner></D:lockinfo>"#;
        // A new path becomes an empty file
        let res = dav.send("LOCK", "/dav/My%20files/Report.docx", &[("timeout", "Second-600")], body).await;
        assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
        let token = res.headers["lock-token"].to_str().unwrap().to_string();
        assert!(token.starts_with("<opaquelocktoken:") && res.body.contains(&token[1..token.len() - 1]));
        assert!(res.body.contains("<D:owner><D:href>Amy</D:href></D:owner>") && res.body.contains("Second-600"));
        assert_eq!(dav.send("GET", "/dav/My%20files/Report.docx", &[], "").await.body, "");
        // A refresh keeps the token
        let res = dav.send("LOCK", "/dav/My%20files/Report.docx", &[("if", &format!("({token})"))], "").await;
        assert_eq!(res.status, StatusCode::OK);
        assert!(res.body.contains(&token[1..token.len() - 1]));
        // Saving while locked, then unlocking
        assert_eq!(dav.send("PUT", "/dav/My%20files/Report.docx", &[("if", &format!("({token})"))], "content").await.status, StatusCode::NO_CONTENT);
        assert_eq!(dav.send("UNLOCK", "/dav/My%20files/Report.docx", &[("lock-token", &token)], "").await.status, StatusCode::NO_CONTENT);

        let body = r#"<?xml version="1.0"?><D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:schemas-microsoft-com:"><D:set><D:prop><Z:Win32LastModifiedTime>Wed, 01 Jan 2026 00:00:00 GMT</Z:Win32LastModifiedTime></D:prop></D:set></D:propertyupdate>"#;
        let res = dav.send("PROPPATCH", "/dav/My%20files/Report.docx", &[], body).await;
        assert_eq!(res.status, StatusCode::MULTI_STATUS);
        assert!(res.body.contains(r#"<R:Win32LastModifiedTime xmlns:R="urn:schemas-microsoft-com:"/></D:prop><D:status>HTTP/1.1 200 OK"#), "{}", res.body);
        assert_eq!(dav.send("PROPPATCH", "/dav/My%20files/Nothing.docx", &[], body).await.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn folder_spaces_and_read_only_spaces_behave_as_on_the_web() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let space = env.folder_space("Server").await;
        let dav = Client::new(&env, &admin, "write").await;
        assert_eq!(dav.send("MKCOL", "/dav/Server/Photos", &[], "").await.status, StatusCode::CREATED);
        assert_eq!(dav.send("PUT", "/dav/Server/Photos/a.txt", &[], "on disk").await.status, StatusCode::CREATED);
        assert_eq!(std::fs::read_to_string(space.dir.join("Photos/a.txt")).unwrap(), "on disk");
        assert_eq!(dav.send("PUT", "/dav/Server/Photos/a.txt", &[], "replaced").await.status, StatusCode::NO_CONTENT);
        assert_eq!(std::fs::read_to_string(space.dir.join("Photos/a.txt")).unwrap(), "replaced");
        assert_eq!(dav.send("GET", "/dav/Server/Photos/a.txt", &[], "").await.body, "replaced");
        assert_eq!(env.node_at(&space.drive, "Photos/a.txt").await.unwrap().1, 8);
        // A file put there on the server shows up
        testutil::write_old(&space.dir.join("Photos/b.txt"), b"b");
        assert!(dav.propfind("/dav/Server/Photos/", "1").await.body.contains("/dav/Server/Photos/b.txt<"));
        let h = [("destination", "/dav/Server/Photos/c.txt")];
        assert_eq!(dav.send("MOVE", "/dav/Server/Photos/b.txt", &h, "").await.status, StatusCode::CREATED);
        assert!(space.dir.join("Photos/c.txt").exists() && !space.dir.join("Photos/b.txt").exists());
        assert_eq!(dav.send("DELETE", "/dav/Server/Photos/c.txt", &[], "").await.status, StatusCode::NO_CONTENT);
        assert!(!space.dir.join("Photos/c.txt").exists());

        sqlx::query("UPDATE drives SET read_only = 1 WHERE id = ?").bind(&space.drive).execute(&env.st.db).await.unwrap();
        assert_eq!(dav.send("PUT", "/dav/Server/Photos/a.txt", &[], "no").await.status, StatusCode::FORBIDDEN);
        assert_eq!(dav.send("MKCOL", "/dav/Server/New", &[], "").await.status, StatusCode::FORBIDDEN);
        assert_eq!(dav.send("DELETE", "/dav/Server/Photos/a.txt", &[], "").await.status, StatusCode::FORBIDDEN);
        assert_eq!(dav.send("GET", "/dav/Server/Photos/a.txt", &[], "").await.body, "replaced");
    }

    #[test]
    fn dates_and_paths() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(784_111_777), "1994-11-06T08:49:37Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(http_date(784_111_777), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(segments("/dav/My%20files//a%2Bb/"), Some(vec!["My files".to_string(), "a+b".to_string()]));
        assert_eq!(segments("/dav"), Some(vec![]));
        assert_eq!(segments("/davx/a"), None);
        assert_eq!(segments("/dav/a/%2e%2e/b"), None);
        assert_eq!(segments("/dav/a%2Fb"), None);
        assert_eq!(href(&["My files".into(), "Ünïcode & co.txt".into()], false), "/dav/My%20files/%C3%9Cn%C3%AFcode%20%26%20co.txt");
        assert_eq!(parse(b"").unwrap().want(), Want::All);
        assert_eq!(parse(br#"<D:propfind xmlns:D="DAV:"><D:propname/></D:propfind>"#).unwrap().want(), Want::Names);
        assert!(parse(b"<not xml").is_err() || parse(b"<a><b></a>").is_err());
    }
}
