//! Branding: site name, logo, theme colors (one set each for light and dark), default appearance mode, sign-in page text.
//!
//! The public `/api/branding`, `/api/branding.css` and `/api/branding/logo` need no sign-in (the sign-in page and public share pages use them too);
//! the index HTML embeds the settings and color stylesheet directly, so the default colors don't flash while loading.

use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    auth::Admin,
    db::{get_setting, set_setting},
    error::{AppError, AppResult},
    logs,
    state::AppState,
    util::now,
};

pub const DEFAULT_NAME: &str = "ThirtyFile";
const DEFAULT_LIGHT: &str = "#2563eb";
const DEFAULT_DARK: &str = "#4f8bff";
const MAX_LOGO: usize = 1024 * 1024;
pub const MAX_BACKGROUND: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Branding {
    /// Site name: browser tab title, top-left corner, sign-in page, public share pages
    pub site_name: String,
    /// Whether to show the site name next to the logo (can be turned off when the logo already contains text)
    pub show_name: bool,
    /// Accent color (buttons, selection, links): one each for light and dark mode
    pub light_brand: String,
    pub dark_brand: String,
    /// Default appearance: system (follow the system), light, dark
    pub default_mode: String,
    /// Users can switch between light / dark themselves
    pub allow_toggle: bool,
    pub login_title: String,
    pub login_subtitle: String,
    pub login_footer: String,
    /// The sign-in page first shows a lock screen (clock and date); the sign-in form appears after pressing any key or clicking
    pub login_lock: bool,
    /// The uploaded logo (light mode; also used in dark mode when no separate one is set)
    pub logo: Option<LogoFile>,
    pub logo_dark: Option<LogoFile>,
    /// Sign-in page background image (without one, a gradient is generated from the accent color)
    pub login_background: Option<LogoFile>,
    /// Updated on every change so browsers reload the logo and styles
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogoFile {
    pub file: String,
    pub mime: String,
}

impl Default for Branding {
    fn default() -> Self {
        Self {
            site_name: DEFAULT_NAME.into(),
            show_name: true,
            light_brand: DEFAULT_LIGHT.into(),
            dark_brand: DEFAULT_DARK.into(),
            default_mode: "system".into(),
            allow_toggle: true,
            login_title: String::new(),
            login_subtitle: "Sign in to access your files".into(),
            login_footer: String::new(),
            login_lock: true,
            logo: None,
            logo_dark: None,
            login_background: None,
            version: 0,
        }
    }
}

pub async fn load(db: &sqlx::SqlitePool) -> Branding {
    match get_setting(db, "branding").await {
        Ok(Some(v)) => serde_json::from_str(&v).unwrap_or_default(),
        _ => Branding::default(),
    }
}

async fn save(st: &AppState, b: &Branding, detail: &str, user: &crate::auth::User) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    set_setting(&mut tx, "branding", &serde_json::to_string(b).unwrap()).await?;
    logs::record_activity(&mut tx, user, None, "settings", detail).await?;
    tx.commit().await?;
    *st.branding.write().unwrap() = b.clone();
    Ok(())
}

// ───────────── Public ─────────────

/// Settings for the frontend (without file paths)
pub fn public_json(b: &Branding) -> Value {
    json!({
        "site_name": b.site_name,
        "show_name": b.show_name,
        "light_brand": b.light_brand,
        "dark_brand": b.dark_brand,
        "default_mode": b.default_mode,
        "allow_toggle": b.allow_toggle,
        "login_title": b.login_title,
        "login_subtitle": b.login_subtitle,
        "login_footer": b.login_footer,
        "login_lock": b.login_lock,
        "has_login_background": b.login_background.is_some(),
        "has_logo": b.logo.is_some(),
        "has_logo_dark": b.logo_dark.is_some(),
        "version": b.version,
    })
}

pub async fn get(State(st): State<AppState>) -> Json<Value> {
    Json(public_json(&st.branding.read().unwrap()))
}

pub async fn css(State(st): State<AppState>) -> Response {
    let body = palette_css(&st.branding.read().unwrap());
    // The page links the stylesheet with ?v=<version>, which changes whenever the branding changes: safe to cache for good
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], body).into_response()
}

/// Web app manifest, so the site can be added to a phone's home screen and opens there without the browser's bars.
/// It's fetched from this origin, which the page's Content-Security-Policy allows (`default-src 'self'`).
pub async fn manifest(State(st): State<AppState>) -> Response {
    let body = manifest_json(&st.branding.read().unwrap()).to_string();
    ([(header::CONTENT_TYPE, "application/manifest+json"), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

/// Named after the site; the icon is the uploaded logo, else the default one (both scale, so one icon fits every size)
fn manifest_json(b: &Branding) -> Value {
    let icon = match &b.logo {
        Some(logo) => json!({ "src": format!("/api/branding/logo?v={}", b.version), "sizes": "any", "type": logo.mime }),
        None => json!({ "src": "/favicon.svg", "sizes": "any", "type": "image/svg+xml" }),
    };
    json!({
        "name": b.site_name,
        "short_name": b.site_name,
        "start_url": "/",
        "scope": "/",
        "display": "standalone",
        "background_color": "#ffffff",
        "theme_color": b.light_brand,
        "icons": [icon],
    })
}

/// Generates CSS variables from the accent color; button text, focus ring and selection background are all derived from it, computed separately for light and dark.
/// The focus ring is the accent color itself: a tint of it would fall below 3:1 against the page
pub fn palette_css(b: &Branding) -> String {
    let mut out = String::new();
    for (selector, color, default, bg, sel) in
        [(":root", &b.light_brand, DEFAULT_LIGHT, "#ffffff", 13), (".dark", &b.dark_brand, DEFAULT_DARK, "#1c1c1e", 24)]
    {
        if color.eq_ignore_ascii_case(default) || !is_hex_color(color) {
            continue;
        }
        let fg = if luminance(color) > 0.19 { "#111111" } else { "#ffffff" };
        out.push_str(&format!(
            "{selector}{{--brand:{color};--brand-foreground:{fg};--ring:{color};--selection:color-mix(in srgb,{color} {sel}%,{bg})}}\n"
        ));
    }
    out
}

fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// sRGB relative luminance (0 black – 1 white); above 0.19, #111111 text has higher contrast than white
fn luminance(hex: &str) -> f64 {
    let ch = |i: usize| {
        let v = u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * ch(1) + 0.7152 * ch(3) + 0.0722 * ch(5)
}

#[derive(Deserialize)]
pub struct LogoQuery {
    #[serde(default)]
    dark: bool,
}

pub async fn logo(State(st): State<AppState>, Query(q): Query<LogoQuery>) -> AppResult<Response> {
    let logo = {
        let b = st.branding.read().unwrap();
        if q.dark { b.logo_dark.clone().or_else(|| b.logo.clone()) } else { b.logo.clone() }
    };
    let Some(logo) = logo else { return Err(AppError::not_found("No logo has been set")) };
    serve(&st, &logo).await
}

pub async fn background(State(st): State<AppState>) -> AppResult<Response> {
    let bg = st.branding.read().unwrap().login_background.clone();
    let Some(bg) = bg else { return Err(AppError::not_found("No sign-in page background has been set")) };
    serve(&st, &bg).await
}

async fn serve(st: &AppState, logo: &LogoFile) -> AppResult<Response> {
    let data = tokio::fs::read(logo_dir(st).join(&logo.file)).await.map_err(|_| AppError::not_found("The image file doesn't exist"))?;
    let mut res = Response::new(axum::body::Body::from(data));
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(&logo.mime).unwrap());
    // The URL carries a version number, so it changes whenever the content does
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    // SVGs may contain scripts: don't run them even when opened directly
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"));
    Ok(res)
}

// ───────────── Administration ─────────────

#[derive(Deserialize)]
pub struct BrandingReq {
    site_name: String,
    show_name: bool,
    light_brand: String,
    dark_brand: String,
    default_mode: String,
    allow_toggle: bool,
    login_title: String,
    login_subtitle: String,
    login_footer: String,
    #[serde(default = "yes")]
    login_lock: bool,
}

fn yes() -> bool {
    true
}

fn check_len(label: &str, s: &str, max: usize) -> AppResult<()> {
    if s.chars().count() > max { Err(AppError::bad_request(format!("{label} can be at most {max} characters"))) } else { Ok(()) }
}

pub async fn update(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<BrandingReq>) -> AppResult<Json<Value>> {
    let site_name = req.site_name.trim();
    if site_name.is_empty() {
        return Err(AppError::bad_request("Enter a site name"));
    }
    check_len("Site name", site_name, 40)?;
    check_len("Sign-in page title", &req.login_title, 60)?;
    check_len("Sign-in page description", &req.login_subtitle, 200)?;
    check_len("Sign-in page footer", &req.login_footer, 500)?;
    for (label, c) in [("Light mode accent color", &req.light_brand), ("Dark mode accent color", &req.dark_brand)] {
        if !is_hex_color(c) {
            return Err(AppError::bad_request(format!("{label} is invalid. Example: #2563eb")));
        }
    }
    if !matches!(req.default_mode.as_str(), "system" | "light" | "dark") {
        return Err(AppError::bad_request("Invalid default appearance"));
    }
    let mut b = st.branding.read().unwrap().clone();
    b.site_name = site_name.to_string();
    b.show_name = req.show_name;
    b.light_brand = req.light_brand.to_ascii_lowercase();
    b.dark_brand = req.dark_brand.to_ascii_lowercase();
    b.default_mode = req.default_mode;
    b.allow_toggle = req.allow_toggle;
    b.login_title = req.login_title.trim().to_string();
    b.login_subtitle = req.login_subtitle.trim().to_string();
    b.login_footer = req.login_footer.trim().to_string();
    b.login_lock = req.login_lock;
    b.version = now();
    let mode = match b.default_mode.as_str() {
        "light" => "Light",
        "dark" => "Dark",
        _ => "Use system setting",
    };
    let detail = format!(
        "Branding: {}, accent colors {} / {}, default appearance: {}{}",
        b.site_name,
        b.light_brand,
        b.dark_brand,
        mode,
        if b.allow_toggle { "" } else { " (users can't switch)" }
    );
    save(&st, &b, &detail, &user).await?;
    Ok(Json(public_json(&b)))
}

fn logo_dir(st: &AppState) -> std::path::PathBuf {
    st.data_dir.join("branding")
}

/// Detects the image format from the content (without trusting the file extension or Content-Type)
fn sniff_image(data: &[u8]) -> Option<(&'static str, &'static str)> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(("image/png", "png"));
    }
    if data.starts_with(b"\xff\xd8\xff") {
        return Some(("image/jpeg", "jpg"));
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some(("image/gif", "gif"));
    }
    if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Some(("image/webp", "webp"));
    }
    if data.starts_with(b"\0\0\x01\0") {
        return Some(("image/x-icon", "ico"));
    }
    let head = String::from_utf8_lossy(&data[..data.len().min(1024)]).to_ascii_lowercase();
    let head = head.trim_start_matches('\u{feff}').trim_start();
    if (head.starts_with("<svg") || head.starts_with("<?xml") || head.starts_with("<!--")) && head.contains("<svg") {
        return Some(("image/svg+xml", "svg"));
    }
    None
}

fn variant(v: &str) -> AppResult<bool> {
    match v {
        "light" => Ok(false),
        "dark" => Ok(true),
        _ => Err(AppError::not_found("Item not found")),
    }
}

pub async fn upload_logo(State(st): State<AppState>, Admin(user): Admin, Path(v): Path<String>, body: Bytes) -> AppResult<Json<Value>> {
    let dark = variant(&v)?;
    if body.is_empty() {
        return Err(AppError::bad_request("The file is empty"));
    }
    if body.len() > MAX_LOGO {
        return Err(AppError::bad_request("The logo file can't be larger than 1 MB"));
    }
    let (mime, ext) = sniff_image(&body).ok_or_else(|| AppError::bad_request("Only PNG, JPG, SVG, WebP, GIF, and ICO images are supported"))?;
    let dir = logo_dir(&st);
    tokio::fs::create_dir_all(&dir).await?;
    let ts = now();
    let file = format!("logo-{}-{ts}.{ext}", if dark { "dark" } else { "light" });
    tokio::fs::write(dir.join(&file), &body).await?;

    let mut b = st.branding.read().unwrap().clone();
    let old = if dark { b.logo_dark.replace(LogoFile { file, mime: mime.into() }) } else { b.logo.replace(LogoFile { file, mime: mime.into() }) };
    b.version = ts;
    save(&st, &b, if dark { "Updated dark mode logo" } else { "Updated logo" }, &user).await?;
    if let Some(old) = old {
        let _ = tokio::fs::remove_file(dir.join(old.file)).await;
    }
    Ok(Json(public_json(&b)))
}

pub async fn delete_logo(State(st): State<AppState>, Admin(user): Admin, Path(v): Path<String>) -> AppResult<Json<Value>> {
    let dark = variant(&v)?;
    let mut b = st.branding.read().unwrap().clone();
    let old = if dark { b.logo_dark.take() } else { b.logo.take() };
    b.version = now();
    save(&st, &b, if dark { "Removed dark mode logo" } else { "Removed logo" }, &user).await?;
    if let Some(old) = old {
        let _ = tokio::fs::remove_file(logo_dir(&st).join(old.file)).await;
    }
    Ok(Json(public_json(&b)))
}

/// Sign-in page background: only raster images (PNG, JPG, WebP, GIF) up to 5 MB
pub async fn upload_background(State(st): State<AppState>, Admin(user): Admin, body: Bytes) -> AppResult<Json<Value>> {
    if body.is_empty() {
        return Err(AppError::bad_request("The file is empty"));
    }
    if body.len() > MAX_BACKGROUND {
        return Err(AppError::bad_request("The background image can't be larger than 5 MB"));
    }
    let (mime, ext) = sniff_image(&body)
        .filter(|(m, _)| matches!(*m, "image/png" | "image/jpeg" | "image/webp" | "image/gif"))
        .ok_or_else(|| AppError::bad_request("Only PNG, JPG, WebP, and GIF images are supported for the background"))?;
    let dir = logo_dir(&st);
    tokio::fs::create_dir_all(&dir).await?;
    let ts = now();
    let file = format!("login-bg-{ts}.{ext}");
    tokio::fs::write(dir.join(&file), &body).await?;

    let mut b = st.branding.read().unwrap().clone();
    let old = b.login_background.replace(LogoFile { file, mime: mime.into() });
    b.version = ts;
    save(&st, &b, "Updated sign-in page background", &user).await?;
    if let Some(old) = old {
        let _ = tokio::fs::remove_file(dir.join(old.file)).await;
    }
    Ok(Json(public_json(&b)))
}

pub async fn delete_background(State(st): State<AppState>, Admin(user): Admin) -> AppResult<Json<Value>> {
    let mut b = st.branding.read().unwrap().clone();
    let old = b.login_background.take();
    b.version = now();
    save(&st, &b, "Removed sign-in page background", &user).await?;
    if let Some(old) = old {
        let _ = tokio::fs::remove_file(logo_dir(&st).join(old.file)).await;
    }
    Ok(Json(public_json(&b)))
}

// ───────────── Index HTML ─────────────

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Injects branding into index.html: title, favicon, settings (for choosing the theme and language before load) and the color stylesheet.
/// `default_lang` is the system default interface language ("auto" = follow the browser).
pub fn inject(html: &str, b: &Branding, default_lang: &str) -> String {
    // The JSON goes inside <script>: escape < so the content can't close the tag early
    let data = public_json(b).to_string().replace('<', "\\u003c");
    let lang = if default_lang == "auto" { String::new() } else { format!("window.__TF_DEFAULT_LANG__={};", json!(default_lang)).replace('<', "\\u003c") };
    let mut out = html.replacen("<head>", &format!("<head>\n    <script>{lang}window.__TF_BRANDING__={data}</script>"), 1);
    if let Some(start) = out.find("<title>")
        && let Some(end) = out[start..].find("</title>")
    {
        out.replace_range(start..start + end + 8, &format!("<title>{}</title>", escape_html(&b.site_name)));
    }
    if b.logo.is_some() {
        out = out.replacen(
            r#"<link rel="icon" type="image/svg+xml" href="/favicon.svg" />"#,
            &format!(r#"<link rel="icon" href="/api/branding/logo?v={}" />"#, b.version),
            1,
        );
    }
    out.replacen("</head>", &format!("  <link rel=\"stylesheet\" id=\"tf-brand-css\" href=\"/api/branding.css?v={}\" />\n  </head>", b.version), 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn req(name: &str, light: &str) -> BrandingReq {
        BrandingReq {
            site_name: name.into(),
            show_name: true,
            light_brand: light.into(),
            dark_brand: "#a78bfa".into(),
            default_mode: "dark".into(),
            allow_toggle: false,
            login_title: "Welcome".into(),
            login_subtitle: "Sign in with your company account".into(),
            login_footer: "</script><b>x</b>".into(),
            login_lock: false,
        }
    }

    #[tokio::test]
    async fn branding_is_saved_validated_and_injected() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let Json(v) = update(State(env.st.clone()), Admin(admin.clone()), Json(req("Stellar Cloud", "#7C3AED"))).await.unwrap();
        assert_eq!(v["light_brand"], "#7c3aed");
        assert_eq!(load(&env.st.db).await.site_name, "Stellar Cloud");
        assert!(update(State(env.st.clone()), Admin(admin.clone()), Json(req("", "#7c3aed"))).await.is_err());
        assert!(update(State(env.st.clone()), Admin(admin.clone()), Json(req("x", "red"))).await.is_err());

        let b = env.st.branding.read().unwrap().clone();
        let css = palette_css(&b);
        assert!(css.contains(":root{--brand:#7c3aed;--brand-foreground:#ffffff;--ring:#7c3aed;"), "the focus ring is the accent color at full strength");
        assert!(css.contains(".dark{--brand:#a78bfa;--brand-foreground:#111111"), "a light accent color gets dark text");
        // Default colors produce no output, keeping the original styles
        assert_eq!(palette_css(&Branding::default()), "");

        let html = inject(
            "<html><head>\n<link rel=\"icon\" type=\"image/svg+xml\" href=\"/favicon.svg\" />\n<title>ThirtyFile</title>\n</head><body></body></html>",
            &b,
            "en",
        );
        assert!(html.contains("<title>Stellar Cloud</title>"));
        assert!(html.contains(r#"<script>window.__TF_DEFAULT_LANG__="en";window.__TF_BRANDING__="#));
        assert!(html.contains("tf-brand-css"));
        assert!(!html.contains("</script><b>"), "settings content must not escape the script tag");
    }

    #[test]
    fn manifest_takes_the_site_name_and_logo() {
        let mut b = Branding { site_name: "Stellar Cloud".into(), ..Branding::default() };
        let m = manifest_json(&b);
        assert_eq!(m["name"], "Stellar Cloud");
        assert_eq!(m["display"], "standalone");
        assert_eq!(m["icons"][0]["src"], "/favicon.svg", "the default icon without a logo");
        b.logo = Some(LogoFile { file: "logo.png".into(), mime: "image/png".into() });
        b.version = 7;
        let m = manifest_json(&b);
        assert_eq!(m["icons"][0]["src"], "/api/branding/logo?v=7");
        assert_eq!(m["icons"][0]["type"], "image/png");
    }

    #[tokio::test]
    async fn logo_upload_checks_the_content() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let png = Bytes::from_static(b"\x89PNG\r\n\x1a\n0000");
        let Json(v) = upload_logo(State(env.st.clone()), Admin(admin.clone()), Path("light".into()), png).await.unwrap();
        assert_eq!(v["has_logo"], true);
        let res = logo(State(env.st.clone()), Query(LogoQuery { dark: true })).await.unwrap();
        assert_eq!(res.headers()[header::CONTENT_TYPE], "image/png", "the light logo is used when there is no dark one");

        let svg = Bytes::from_static(b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"></svg>");
        let Json(v) = upload_logo(State(env.st.clone()), Admin(admin.clone()), Path("dark".into()), svg).await.unwrap();
        assert_eq!(v["has_logo_dark"], true);
        let res = logo(State(env.st.clone()), Query(LogoQuery { dark: true })).await.unwrap();
        assert!(res.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("sandbox"));

        let html = Bytes::from_static(b"<html><script>alert(1)</script></html>");
        assert!(upload_logo(State(env.st.clone()), Admin(admin.clone()), Path("light".into()), html).await.is_err());

        let _ = delete_logo(State(env.st.clone()), Admin(admin), Path("light".into())).await.unwrap();
        assert!(logo(State(env.st.clone()), Query(LogoQuery { dark: false })).await.is_err());
        assert_eq!(std::fs::read_dir(env.dir.join("branding")).unwrap().count(), 1, "the old file was removed");
    }

    #[tokio::test]
    async fn login_background_accepts_only_bitmaps() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        assert!(background(State(env.st.clone())).await.is_err());
        let svg = Bytes::from_static(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>");
        assert!(upload_background(State(env.st.clone()), Admin(admin.clone()), svg).await.is_err(), "the background doesn't accept SVG");
        let jpg = Bytes::from_static(b"\xff\xd8\xff\xe0rest");
        let Json(v) = upload_background(State(env.st.clone()), Admin(admin.clone()), jpg).await.unwrap();
        assert_eq!(v["has_login_background"], true);
        let res = background(State(env.st.clone())).await.unwrap();
        assert_eq!(res.headers()[header::CONTENT_TYPE], "image/jpeg");
        let Json(v) = delete_background(State(env.st.clone()), Admin(admin)).await.unwrap();
        assert_eq!(v["has_login_background"], false);
        assert_eq!(std::fs::read_dir(env.dir.join("branding")).unwrap().count(), 0);
    }
}
