//! Paths below /dav: decoding them, links, and the Destination header

use super::*;

/// The decoded path segments below `/dav`; None for a path outside it or with `.`/`..`
pub(super) fn segments(path: &str) -> Option<Vec<String>> {
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
pub(super) fn href(segs: &[String], collection: bool) -> String {
    let mut s = format!("{PREFIX}/");
    s.push_str(&segs.iter().map(|x| utf8_percent_encode(x, SEGMENT).to_string()).collect::<Vec<_>>().join("/"));
    if collection && !segs.is_empty() {
        s.push('/');
    }
    s
}

/// The Destination of a MOVE or COPY: a full URL or a path
pub(super) fn destination(headers: &HeaderMap) -> AppResult<Vec<String>> {
    let raw = headers.get("destination").and_then(|v| v.to_str().ok()).ok_or_else(|| AppError::bad_request("Missing Destination"))?;
    let path = match raw.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]),
        None => raw,
    };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    segments(path).ok_or_else(|| AppError::new(StatusCode::BAD_GATEWAY, "The destination isn't on this WebDAV server"))
}
