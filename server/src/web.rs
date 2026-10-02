//! Embedded frontend (web/dist). Bundled into the executable in release builds; read from disk in debug builds.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, Uri, header},
    response::{Html, IntoResponse, Response},
};
use base64::Engine;
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};

use crate::i18n::{self, Lang};

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

/// What the page is told about its language, decided for each request (i18n/):
/// - `__TF_LANG__`: the language the person (or, for someone signed in, the system default) picked; absent when the
///   browser's languages decide. The page takes it first, unless it doesn't offer it
/// - `__TF_DEFAULT_LANG__`: the system default, for a visitor whose browser has none of the languages offered
/// - `<html lang>`, and the dictionary script (`<lang>.js`, from web/src/lib/i18n/<lang>/) of the language the server
///   expects the page to use, so it needn't fetch it. The page decides the browser's part itself (it also offers the
///   languages being previewed), and loads its own dictionary when this guessed wrong
struct PageLang {
    picked: Option<Lang>,
    default: Option<Lang>,
    expected: Lang,
}

impl PageLang {
    async fn of(st: &crate::state::AppState, headers: &HeaderMap) -> PageLang {
        let visitor = i18n::visitor(st, headers).await;
        let default = i18n::system_default(st);
        PageLang { picked: i18n::picked(headers, visitor, default), default, expected: i18n::resolve(headers, visitor, default) }
    }

    /// The script setting the page's globals (codes only, so nothing needs escaping)
    fn script(&self) -> String {
        let mut js = String::new();
        if let Some(l) = self.picked {
            js.push_str(&format!("window.__TF_LANG__=\"{}\";", l.code()));
        }
        if let Some(l) = self.default {
            js.push_str(&format!("window.__TF_DEFAULT_LANG__=\"{}\";", l.code()));
        }
        js
    }

    /// The dictionary script to put on the page: English is the source text and has none
    fn dictionary(&self) -> Option<String> {
        (self.expected != Lang::En).then(|| format!("{}.js", self.expected.code()))
    }

    /// index.html with `<html lang>` set to the expected language
    fn tag(&self, html: &str) -> String {
        const OPEN: &str = "<html lang=\"";
        let Some(start) = html.find(OPEN).map(|i| i + OPEN.len()) else { return html.to_string() };
        let Some(len) = html[start..].find('"') else { return html.to_string() };
        let mut out = html.to_string();
        out.replace_range(start..start + len, self.expected.html_tag());
        out
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
            let lang = PageLang::of(&st, &headers).await;
            let page = lang.tag(&String::from_utf8_lossy(&index.data));
            let mut html = crate::branding::inject(&page, &st.part::<crate::branding::Memory>().settings.read().unwrap(), &lang.script());
            if let Some(dict) = lang.dictionary()
                && Assets::get(&dict).is_some()
            {
                html = html.replacen("</head>", &format!("  <script src=\"/{dict}\"></script>\n  </head>"), 1);
            }
            let csp = content_security_policy(&html);
            // The page differs by who asks: their session and language cookies, and their browser's languages
            let vary = (header::VARY, "Cookie, Accept-Language".to_string());
            ([(header::CACHE_CONTROL, "no-cache".to_string()), (header::CONTENT_SECURITY_POLICY, csp), vary], SECURITY_HEADERS, Html(html)).into_response()
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

    fn with(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(k.parse::<header::HeaderName>().unwrap(), v.parse().unwrap());
        }
        h
    }

    #[tokio::test]
    async fn the_page_carries_the_language_of_whoever_asks() {
        let env = crate::testutil::env().await;
        let amy = env.user("amy", true).await;
        let (_, session) = env.sign_in(&amy, "Firefox").await;
        env.st.system.write().unwrap().default_lang = "en".into();
        let page = |h: HeaderMap| {
            let st = env.st.clone();
            async move { PageLang::of(&st, &h).await }
        };

        // Not signed in: the browser's language before the system default, which the page gets in case none fits
        let p = page(with(&[("accept-language", "zh-TW,zh;q=0.9")])).await;
        assert_eq!((p.picked, p.expected), (None, Lang::ZhTw));
        assert_eq!(p.script(), r#"window.__TF_DEFAULT_LANG__="en";"#);
        assert_eq!(p.dictionary().as_deref(), Some("zh-TW.js"));
        assert_eq!(p.tag(r#"<!doctype html><html lang="en"><head>"#), r#"<!doctype html><html lang="zh-Hant"><head>"#);
        // A language chosen in this browser
        let p = page(with(&[("accept-language", "en"), ("cookie", "tf_lang=zh-CN; tf_lang_chosen=1")])).await;
        assert_eq!((p.picked, p.expected), (Some(Lang::ZhCn), Lang::ZhCn));
        assert_eq!(p.script(), r#"window.__TF_LANG__="zh-CN";window.__TF_DEFAULT_LANG__="en";"#);

        // Signed in: the system default before the browser, and the language saved with the account before both
        let p = page(with(&[("accept-language", "zh-TW"), ("cookie", &session)])).await;
        assert_eq!((p.picked, p.expected), (Some(Lang::En), Lang::En));
        assert_eq!(p.dictionary(), None);
        sqlx::query("UPDATE users SET chosen_lang = 'ja' WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        let p = page(with(&[("accept-language", "zh-TW"), ("cookie", &format!("{session}; tf_lang=zh-TW; tf_lang_chosen=1"))])).await;
        assert_eq!((p.picked, p.expected), (Some(Lang::Ja), Lang::Ja));
        assert_eq!(p.dictionary().as_deref(), Some("ja.js"));
        assert_eq!(p.tag(r#"<html lang="zh-Hant">"#), r#"<html lang="ja">"#);
    }

    #[tokio::test]
    async fn only_known_languages_reach_the_page() {
        // The cookies are the visitor's own text: anything but a known language's code is no choice
        let env = crate::testutil::env().await;
        for cookie in ["tf_lang=\"><script>x</script>; tf_lang_chosen=1", "tf_lang=../index.html; tf_lang_chosen=1", "tf_lang=fr; tf_lang_chosen=1"] {
            let p = PageLang::of(&env.st, &with(&[("cookie", cookie)])).await;
            assert_eq!((p.picked, p.expected, p.script(), p.dictionary()), (None, Lang::En, String::new(), None), "{cookie}");
        }
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
