//! Embedded frontend (web/dist). Bundled into the executable in release builds; read from disk in debug builds.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, Uri, header},
    response::{Html, IntoResponse, Response},
};
use base64::Engine;
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};

#[derive(RustEmbed)]
#[folder = "../web/dist"]
#[allow_missing = true]
struct Assets;

/// Security headers for the web pages themselves:
/// - Only this site may embed them (prevents clickjacking; the sandboxed iframe for Office previews uses srcdoc, so it's unaffected)
/// - No referrer URL is sent to other sites (share page URLs contain the share token)
/// - The Content-Security-Policy is built per page (see `content_security_policy`)
const SECURITY_HEADERS: [(header::HeaderName, &str); 3] =
    [(header::X_FRAME_OPTIONS, "SAMEORIGIN"), (header::REFERRER_POLICY, "same-origin"), (header::X_CONTENT_TYPE_OPTIONS, "nosniff")];

/// Content-Security-Policy for the app page. Scripts may only come from this site, plus the inline scripts we generate
/// ourselves, allowed by hash: the theme bootstrap in index.html, the injected branding settings, and the Office previewer
/// (`office-frame.js`), which the page inlines into a sandboxed srcdoc iframe; srcdoc documents inherit this policy,
/// so that script has to be allowed here too. Everything else (styles, images, fonts, API calls) stays on this origin;
/// blob:/data: are needed for previews and downloads generated in the browser.
fn content_security_policy(html: &str) -> String {
    let mut hashes = inline_script_hashes(html);
    hashes.extend(office_frame_hash().iter().cloned());
    format!(
        "default-src 'self'; script-src 'self' {}; style-src 'self' 'unsafe-inline'; img-src 'self' blob: data:; \
         media-src 'self' blob:; font-src 'self' blob: data:; connect-src 'self'; worker-src 'self' blob:; \
         frame-src 'self' blob:; object-src 'self'; base-uri 'self'; form-action 'self'; frame-ancestors 'self'",
        hashes.join(" ")
    )
}

/// Whether the page should carry the Traditional Chinese dictionary: the visitor's saved language, else the system
/// default, else the browser's preferred language. The frontend decides the same way, so this only saves a round trip
fn wants_chinese(headers: &HeaderMap, default_lang: &str) -> bool {
    let cookie = crate::auth::get_cookie(headers, "tf_lang");
    // `tf_lang_chosen` is set only when the person picked a language themselves; without it `tf_lang` just mirrors
    // what the page last decided, so the system default (when fixed) takes precedence, like in the frontend
    if crate::auth::get_cookie(headers, "tf_lang_chosen").is_some()
        && let Some(saved) = cookie
    {
        return saved == "zh-TW";
    }
    match default_lang {
        "zh-TW" => true,
        "en" => false,
        _ if cookie.is_some() => cookie == Some("zh-TW"),
        _ => headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .is_some_and(|first| first.trim().to_ascii_lowercase().starts_with("zh")),
    }
}

/// `'sha256-…'` sources for every inline `<script>` (without src) in the HTML
fn inline_script_hashes(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        let tag_end = match rest[start..].find('>') {
            Some(i) => start + i + 1,
            None => break,
        };
        let tag = &rest[start..tag_end];
        let body_end = match rest[tag_end..].find("</script") {
            Some(i) => tag_end + i,
            None => break,
        };
        if !tag.contains("src=") {
            out.push(script_hash(&rest[tag_end..body_end]));
        }
        rest = &rest[body_end..];
    }
    out
}

/// Hash of a script as the browser runs it. HTML parsing turns CRLF and lone CR into LF (and NUL into U+FFFD) before
/// the script is compared with the policy, so an index.html built from a Windows checkout, with CRLF line endings,
/// must be hashed after the same changes.
fn script_hash(script: &str) -> String {
    let parsed = script.replace("\r\n", "\n").replace('\r', "\n").replace('\0', "\u{FFFD}");
    format!("'sha256-{}'", base64::engine::general_purpose::STANDARD.encode(Sha256::digest(parsed.as_bytes())))
}

/// Hash of office-frame.js exactly as the page inlines it into the iframe (`</script` is escaped as `<\/script` there).
/// Computed once: the file is part of the build.
fn office_frame_hash() -> &'static Option<String> {
    static HASH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HASH.get_or_init(|| {
        let file = Assets::get("office-frame.js")?;
        let js = String::from_utf8_lossy(&file.data);
        Some(script_hash(&escape_script_end(&js)))
    })
}

/// Same transformation as the frontend's `script.replace(/<\/(script)/gi, "<\\/$1")`
fn escape_script_end(js: &str) -> String {
    let lower = js.to_ascii_lowercase();
    let mut out = String::with_capacity(js.len());
    let mut last = 0;
    let mut search = 0;
    while let Some(i) = lower[search..].find("</script") {
        let at = search + i;
        out.push_str(&js[last..at]);
        out.push_str("<\\/");
        last = at + 2;
        search = at + 8;
    }
    out.push_str(&js[last..]);
    out
}

pub async fn serve(State(st): State<crate::state::AppState>, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return crate::error::AppError::not_found("API not found").into_response();
    }
    if !path.is_empty()
        && let Some(file) = Assets::get(path)
    {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        // Vite's assets/ file names contain a hash, so they can be cached long-term; other files (e.g. office-frame.js) are revalidated every time, returning 304 if unchanged
        let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
        let etag = format!("\"{}\"", hex::encode(&file.metadata.sha256_hash()[..16]));
        if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
            return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag), (header::CACHE_CONTROL, cache.to_string())]).into_response();
        }
        return (
            [
                (header::CONTENT_TYPE, mime.as_ref().to_string()),
                (header::CACHE_CONTROL, cache.to_string()),
                (header::ETAG, etag),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            ],
            file.data,
        )
            .into_response();
    }
    match Assets::get("index.html") {
        Some(index) => {
            // Inject branding (title, favicon, colors) so the default styling doesn't flash while loading
            let lang = st.system.read().unwrap().default_lang.clone();
            let mut html = crate::branding::inject(&String::from_utf8_lossy(&index.data), &st.branding.read().unwrap(), &lang);
            if wants_chinese(&headers, &lang) && Assets::get("zh-TW.js").is_some() {
                html = html.replacen("</head>", "  <script src=\"/zh-TW.js\"></script>\n  </head>", 1);
            }
            let csp = content_security_policy(&html);
            ([(header::CACHE_CONTROL, "no-cache".to_string()), (header::CONTENT_SECURITY_POLICY, csp)], SECURITY_HEADERS, Html(html)).into_response()
        }
        None => (StatusCode::NOT_FOUND, Html("<p>The frontend hasn't been built yet: run <code>pnpm build</code> in web/ first, or use <code>pnpm dev</code>.</p>"))
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_scripts_are_allowed_by_hash_only() {
        let html = r#"<html><head><script>window.__TF_BRANDING__={"a":1}</script><script type="module" src="/assets/index.js"></script>
            <script>
              try { document.documentElement.classList.add("dark"); } catch {}
            </script></head></html>"#;
        let hashes = inline_script_hashes(html);
        assert_eq!(hashes.len(), 2, "two inline scripts, the external one is not hashed");
        assert_eq!(hashes[0], script_hash(r#"window.__TF_BRANDING__={"a":1}"#));
        let csp = content_security_policy(html);
        assert!(csp.starts_with("default-src 'self'; script-src 'self' 'sha256-"));
        let script_src = csp.split(';').find(|d| d.trim().starts_with("script-src")).unwrap();
        assert!(!script_src.contains("unsafe-inline"), "no unsafe-inline for scripts: {script_src}");
        assert!(csp.contains("frame-ancestors 'self'"));
    }

    #[test]
    fn dictionary_follows_the_system_default_unless_the_person_chose() {
        let with = |pairs: &[(&str, &str)]| {
            let mut h = HeaderMap::new();
            for (k, v) in pairs {
                h.append(k.parse::<header::HeaderName>().unwrap(), v.parse().unwrap());
            }
            h
        };
        // No fixed default: the browser decides, or the language the page last used
        assert!(wants_chinese(&with(&[("accept-language", "zh-TW,zh;q=0.9")]), "auto"));
        assert!(!wants_chinese(&with(&[("accept-language", "en-US")]), "auto"));
        assert!(wants_chinese(&with(&[("accept-language", "en-US"), ("cookie", "tf_lang=zh-TW")]), "auto"));
        // A fixed default wins over what the page mirrored into tf_lang…
        assert!(!wants_chinese(&with(&[("accept-language", "zh-TW"), ("cookie", "tf_lang=zh-TW")]), "en"));
        assert!(wants_chinese(&with(&[("cookie", "tf_lang=en")]), "zh-TW"));
        // …but not over a language the person picked themselves
        assert!(wants_chinese(&with(&[("cookie", "tf_lang=zh-TW; tf_lang_chosen=1")]), "en"));
        assert!(!wants_chinese(&with(&[("cookie", "tf_lang_chosen=1; tf_lang=en")]), "zh-TW"));
    }

    #[test]
    fn line_endings_are_hashed_as_the_browser_parses_them() {
        // base64(sha256("\n  let a = 1;\n  let b = 2;\n")), the script a browser runs from any of these pages
        let want = "'sha256-HZCPBoNcEBwurXrAGF/huOP1Ag8cIJGt2uMjvefHpAQ='";
        for body in ["\n  let a = 1;\n  let b = 2;\n", "\r\n  let a = 1;\r\n  let b = 2;\r\n", "\r  let a = 1;\r  let b = 2;\r", "\r\n  let a = 1;\r  let b = 2;\n"]
        {
            let html = format!("<html><head><script>{body}</script></head></html>");
            assert_eq!(inline_script_hashes(&html), vec![want.to_string()], "{body:?}");
        }
    }

    #[test]
    fn script_end_is_escaped_like_the_frontend() {
        assert_eq!(escape_script_end("a</script>b</SCRIPT>c"), "a<\\/script>b<\\/SCRIPT>c");
        assert_eq!(escape_script_end("no tags"), "no tags");
    }
}
