//! Middleware for every request: which responses are compressed, the headers a reverse proxy adds, and the
//! same-origin check.

use axum::{
    extract::Request,
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tower_http::compression::predicate::Predicate;

use crate::{auth, dav, state::AppState, tokens};

/// Which responses to compress: text-like bodies of at least 1 KB. File downloads (octet-stream, images, video, zip)
/// are already compressed or served with Range requests, and compressing them would break Content-Length and resumable downloads.
#[derive(Clone, Copy)]
pub struct Compressible;

impl Predicate for Compressible {
    fn should_compress<B: axum::body::HttpBody>(&self, response: &axum::http::Response<B>) -> bool {
        // File contents (served with Content-Disposition) are streamed with an exact Content-Length and an ETag per content:
        // compressing them would hide the length (no download progress) and make the ETag ambiguous
        if response.status() == StatusCode::PARTIAL_CONTENT
            || response.headers().contains_key(header::CONTENT_RANGE)
            || response.headers().contains_key(header::CONTENT_DISPOSITION)
        {
            return false;
        }
        // Content-Length isn't set yet at this layer for JSON bodies, so ask the body itself when it knows its size
        let len = response.body().size_hint().exact().or_else(|| {
            response.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok())
        });
        if len.is_some_and(|len| len < 1024) {
            return false;
        }
        let Some(ct) = response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) else { return false };
        let ct = ct.split(';').next().unwrap_or_default().trim();
        ct.starts_with("text/")
            || matches!(ct, "application/json" | "application/javascript" | "application/xml" | "image/svg+xml" | "application/manifest+json")
    }
}

/// What a reverse proxy tells about the request (`X-Forwarded-Host`, `X-Forwarded-Proto`) counts only when it comes from
/// a trusted proxy address (THIRTYFILE_TRUST_PROXY), like the visitor's address in `X-Forwarded-For`: from anyone else
/// the headers are removed before anything reads them. An HTTPS site also tells browsers to use HTTPS only (HSTS).
pub async fn forwarding(axum::extract::State(st): axum::extract::State<AppState>, mut req: Request, next: Next) -> Response {
    let peer = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    if !peer.is_some_and(|p| st.trust_proxy.trusts(p)) {
        req.headers_mut().remove("x-forwarded-host");
        req.headers_mut().remove("x-forwarded-proto");
    }
    let mut res = next.run(req).await;
    if st.https() {
        res.headers_mut().insert(header::STRICT_TRANSPORT_SECURITY, header::HeaderValue::from_static("max-age=31536000"));
    }
    res
}

/// Basic CSRF protection: requests that modify data must have an Origin matching Host, if they carry one
/// (behind a reverse proxy with THIRTYFILE_TRUST_PROXY set, X-Forwarded-Host is accepted too).
/// A request signed in with an app password as a Bearer token and without a session cookie carries no credential a
/// browser adds by itself, so another website can't send it on someone's behalf: it isn't checked. Basic credentials
/// are always checked: WebDAV answers with a sign-in challenge, after which a browser remembers them and may send them
/// by itself, to `/dav` and possibly the rest of the site (WebDAV clients don't send an Origin).
pub async fn same_origin(axum::extract::State(st): axum::extract::State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let dav = path == dav::PREFIX || path.starts_with("/dav/");
    let app_password_only = !dav
        && matches!(tokens::credential(req.headers()), Some(tokens::Credential::Bearer(_)))
        && auth::get_cookie(req.headers(), auth::SESSION_COOKIE).is_none();
    if !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS)
        && !app_password_only
        && let Some(origin) = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            let origin_host = origin.split_once("://").map(|(_, h)| h).unwrap_or(origin);
            let h = req.headers();
            let hosts = [h.get("x-forwarded-host").filter(|_| st.trust_proxy.enabled()), h.get(header::HOST)];
            let ok = hosts.iter().flatten().filter_map(|v| v.to_str().ok()).any(|host| host == origin_host);
            if !ok {
                return (StatusCode::FORBIDDEN, "cross-origin request blocked").into_response();
            }
        }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: StatusCode, headers: &[(header::HeaderName, &str)]) -> axum::http::Response<String> {
        response_of(status, headers, 4096)
    }

    fn response_of(status: StatusCode, headers: &[(header::HeaderName, &str)], body_len: usize) -> axum::http::Response<String> {
        let mut r = axum::http::Response::new("x".repeat(body_len));
        *r.status_mut() = status;
        for (k, v) in headers {
            r.headers_mut().insert(k.clone(), v.parse().unwrap());
        }
        r
    }

    #[test]
    fn only_text_like_full_responses_are_compressed() {
        let ok = |ct: &str| Compressible.should_compress(&response(StatusCode::OK, &[(header::CONTENT_TYPE, ct)]));
        assert!(ok("application/json"));
        assert!(ok("text/html; charset=utf-8"));
        assert!(ok("text/csv; charset=utf-8"));
        assert!(ok("image/svg+xml"));
        assert!(!ok("application/octet-stream"));
        assert!(!ok("application/zip"));
        assert!(!ok("video/mp4"));
        assert!(!ok("image/jpeg"));
        assert!(!Compressible.should_compress(&response(StatusCode::OK, &[(header::CONTENT_TYPE, "text/plain"), (header::CONTENT_DISPOSITION, "inline; filename=\"a.txt\"")])));
        // Range responses and tiny bodies are left alone
        assert!(!Compressible.should_compress(&response(StatusCode::PARTIAL_CONTENT, &[(header::CONTENT_TYPE, "text/plain")])));
        assert!(!Compressible.should_compress(&response_of(StatusCode::OK, &[(header::CONTENT_TYPE, "text/plain")], 20)));
        assert!(Compressible.should_compress(&response_of(StatusCode::OK, &[(header::CONTENT_TYPE, "text/plain")], 4096)));
    }
}
