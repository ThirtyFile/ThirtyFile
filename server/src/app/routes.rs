//! The routes: the API (with or without the request timeout), WebDAV and the web interface.

use std::time::Duration;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    middleware,
    routing::{any, delete, get, head, patch, post, put},
};
use tower_http::{CompressionLevel, compression::CompressionLayer, timeout::TimeoutLayer, trace::TraceLayer};

use crate::{
    admin, archive, auth, backups, branding, dav, downloads, drives, error, files, history, i18n, jobs, location_tools, locations, logs, mail, moves, nodes, notify,
    paths, replicas, reset, sessions, shares, signin, sso, state::AppState, style, tags, thumbnails, tokens, twofactor, upload, usage, versions, web,
};

use super::{
    health::health,
    middleware::{Compressible, forwarding, same_origin},
};

/// All routes: the API (with or without the request timeout) and the web interface
pub fn router(state: AppState) -> Router {
    Router::new()
        .nest(
            "/api",
            api()
                .layer(TimeoutLayer::with_status_code(StatusCode::GATEWAY_TIMEOUT, Duration::from_secs(120)))
                .merge(untimed())
                // The error log records the route that answered, not the path asked for (logs/errors.rs)
                .route_layer(middleware::from_fn(error::note_route)),
        )
        // WebDAV (dav/): outside the request timeout too, as it receives and sends whole files
        .route(dav::PREFIX, any(dav::handle))
        .route("/dav/", any(dav::handle))
        .route("/dav/{*path}", any(dav::handle))
        .route("/", any(dav::server_root))
        .fallback(web::serve)
        .layer(middleware::from_fn_with_state(state.clone(), same_origin))
        .layer(middleware::from_fn_with_state(state.clone(), forwarding))
        // gzip / brotli for JSON, HTML, JS, CSS and SVG (see `Compressible`); file contents and other downloads are never compressed
        // Level 4: brotli's default (11) spends far more CPU per response than it saves on JSON and HTML; gzip 4 is likewise the sweet spot
        .layer(CompressionLayer::new().gzip(true).br(true).quality(CompressionLevel::Precise(4)).compress_when(Compressible))
        .layer(TraceLayer::new_for_http())
        // A bug hit by one request answers that request with an error instead of stopping the server for everyone
        .layer(tower_http::catch_panic::CatchPanicLayer::new())
        // Outermost, so a request that panicked is recorded too: gives each request an id and records the errors
        // people run into (logs/errors.rs)
        .layer(middleware::from_fn_with_state(state.clone(), logs::track))
        .with_state(state)
}

/// Routes outside the API's request timeout (see `router`): they receive content and write it to the storage
/// location, or wait in the thumbnail queue, and legitimately take as long as that takes
fn untimed() -> Router<AppState> {
    // File operations: app passwords work here too
    let files = Router::new()
        .route("/files/{id}/content", put(files::save_content).layer(DefaultBodyLimit::max(files::MAX_EDIT_BYTES)))
        .route(
            "/files/{id}/thumbnail",
            get(thumbnails::thumbnail).merge(put(thumbnails::upload_thumbnail).layer(DefaultBodyLimit::max(thumbnails::MAX_THUMB_UPLOAD))),
        )
        .route("/files/{id}/versions/{version}/restore", post(versions::restore))
        .route("/uploads", post(upload::create).options(upload::options))
        .route("/uploads/{id}", head(upload::head).patch(upload::patch).delete(upload::delete).layer(DefaultBodyLimit::disable()))
        .route_layer(middleware::from_fn(auth::app_passwords::allow));
    Router::new()
        .merge(files)
        .route("/public/shares/{token}/nodes/{id}/thumbnail", get(shares::public_thumbnail))
        // Visitors of a share link that accepts files
        .route("/public/shares/{token}/uploads", post(shares::public_upload_create).options(upload::options))
        .route(
            "/public/shares/{token}/uploads/{id}",
            head(shares::public_upload_head).patch(shares::public_upload_patch).delete(shares::public_upload_delete).layer(DefaultBodyLimit::disable()),
        )
        .route("/admin/branding/logo/{variant}", put(branding::upload_logo).delete(branding::delete_logo))
        // One file of a storage location, as it is stored
        .route("/admin/storage/{id}/download", get(location_tools::download))
        .route(
            "/admin/branding/background",
            put(branding::upload_background).delete(branding::delete_background).layer(DefaultBodyLimit::max(branding::MAX_BACKGROUND + 1024)),
        )
}

/// Routes that also accept app passwords (`Authorization: Bearer` or HTTP Basic, see tokens.rs): file operations only.
/// Everything else (the account itself, sign-in methods, sharing, logs and administration) needs a browser session.
fn file_api() -> Router<AppState> {
    Router::new()
        .route("/auth/me", get(signin::me))
        .route("/nodes/{id}", get(nodes::get).patch(nodes::rename))
        .route("/nodes/{id}/children", get(nodes::children))
        .route("/nodes/{id}/position", get(nodes::position))
        .route("/nodes/{id}/select", post(nodes::select))
        .route("/nodes/move", post(nodes::move_nodes))
        .route("/nodes/copy", post(nodes::copy_nodes))
        .route("/nodes/conflicts", post(nodes::conflicts))
        .route("/nodes/trash", post(nodes::trash))
        .route("/nodes/contents", post(nodes::contents))
        .route("/nodes/find", post(paths::find))
        .route("/folders", post(nodes::create_folder))
        .route("/trash", get(nodes::list_trash))
        .route("/trash/restore", post(nodes::restore))
        .route("/trash/delete", post(nodes::delete_forever))
        .route("/trash/empty", get(nodes::empty_trash_preview).post(nodes::empty_trash))
        .route("/search", get(nodes::search))
        .route("/recent", get(nodes::recent))
        .route("/favorites", get(nodes::favorites))
        .route("/nodes/favorite", post(nodes::set_favorite))
        // Each person's own tags (tags.rs)
        .route("/tags", get(tags::list).post(tags::create))
        .route("/tags/{id}", patch(tags::update).delete(tags::delete))
        .route("/tags/{id}/items", get(nodes::tagged))
        .route("/nodes/tags", post(tags::apply))
        // Each person's own smart folders: saved searches that list like folders (nodes/smart.rs)
        .route("/smart-folders", get(nodes::smart_folders).post(nodes::create_smart_folder))
        .route("/smart-folders/{id}", get(nodes::smart_folder).patch(nodes::update_smart_folder).delete(nodes::delete_smart_folder))
        .route("/smart-folders/{id}/items", get(nodes::smart_items))
        .route("/smart-folders/{id}/position", get(nodes::smart_position))
        .route("/smart-folders/{id}/select", post(nodes::smart_select))
        .route("/shared-with-me", get(nodes::shared_with_me))
        .route("/files/{id}/content", get(files::content))
        .route("/files/{id}/versions", get(versions::list))
        .route("/files/{id}/versions/{version}/content", get(versions::content))
        .route("/download", get(downloads::download).post(downloads::create_download_link))
        .route("/download/{link}", get(downloads::download_by_link))
        .route("/archive/compress", post(archive::compress))
        .route("/archive/extract", post(archive::extract))
        .route("/jobs/{id}", get(jobs::get))
        .route_layer(middleware::from_fn(auth::app_passwords::allow))
}

fn api() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .merge(file_api())
        .merge(account_api())
        .merge(sharing_api())
        .merge(site_api())
        .merge(admin_api())
        .merge(storage_api())
        .fallback(|| async { error::AppError::not_found("API not found") })
}

/// Signing in and out, and the account's own settings: password, devices, app passwords, two-factor sign-in, sign-in
/// methods
fn account_api() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(signin::login))
        .route("/auth/options", get(reset::options))
        .route("/auth/forgot", post(reset::forgot))
        .route("/auth/reset", post(reset::reset))
        .route("/auth/login/2fa", post(twofactor::login_code))
        .route("/auth/login/2fa/setup", post(twofactor::login_setup))
        .route("/auth/logout", post(signin::logout))
        .route("/auth/password", axum::routing::put(signin::change_password))
        .route("/auth/language", put(i18n::save))
        .route("/auth/style", put(style::save))
        .route("/auth/sessions", get(sessions::list))
        .route("/auth/sessions/others", post(sessions::sign_out_others))
        .route("/auth/sessions/{id}", delete(sessions::sign_out))
        .route("/auth/app-passwords", get(tokens::list).post(tokens::create))
        .route("/auth/app-passwords/{id}", delete(tokens::delete))
        .route("/auth/2fa", get(twofactor::status))
        .route("/auth/2fa/setup", post(twofactor::start_setup))
        .route("/auth/2fa/enable", post(twofactor::enable))
        .route("/auth/2fa/disable", post(twofactor::disable))
        .route("/auth/2fa/recovery-codes", post(twofactor::new_recovery_codes_for_me))
        .route("/auth/sso/providers", get(sso::providers))
        .route("/auth/sso/{provider}/start", get(sso::start))
        .route("/auth/sso/{provider}/link", post(sso::start_link))
        .route("/auth/sso/{provider}/callback", get(sso::callback))
        .route("/auth/identities", get(sso::my_identities))
        .route("/auth/identities/{provider}", delete(sso::unlink))
}

/// Spaces, who has access, what happened in them, and share links (with what visitors of a link reach)
fn sharing_api() -> Router<AppState> {
    Router::new()
        // Listing the spaces works with an app password too
        .route("/drives", get(drives::list).layer(middleware::from_fn(auth::app_passwords::allow)).post(drives::create))
        .route("/drives/{id}", patch(drives::update).delete(drives::delete))
        .route("/nodes/{id}/access", get(drives::access).post(drives::grant))
        .route("/grants/{id}", delete(drives::revoke))
        .route("/directory", get(drives::directory))
        .route("/activity", get(history::activity))
        .route("/activity/export", get(history::export_activity))
        .route("/nodes/{id}/activity", get(history::node_history))
        .route("/share-access", get(history::share_access))
        .route("/shares", get(shares::list).post(shares::create))
        .route("/shares/{id}", patch(shares::update).delete(shares::delete))
        .route("/public/shares/{token}", get(shares::public_info))
        .route("/public/shares/{token}/unlock", post(shares::unlock))
        .route("/public/shares/{token}/download", get(shares::public_download).post(shares::create_public_download_link))
        .route("/public/shares/{token}/download/{link}", get(shares::public_download_by_link))
        .route("/public/shares/{token}/nodes/{id}", get(shares::public_node))
        .route("/public/shares/{token}/nodes/{id}/children", get(shares::public_children))
        .route("/public/shares/{token}/nodes/{id}/content", get(shares::public_content))
}

/// The site: its look, notifications, the logs, and email
fn site_api() -> Router<AppState> {
    Router::new()
        .route("/admin/email", get(mail::get_settings).put(mail::update_settings))
        .route("/admin/email/test", post(mail::test))
        .route("/notifications", get(notify::list).delete(notify::clear))
        .route("/notifications/read", post(notify::mark_read))
        .route("/notifications/settings", get(notify::get_settings).put(notify::update_settings))
        .route("/notifications/{id}", delete(notify::delete))
        .route("/branding", get(branding::get))
        .route("/branding.css", get(branding::css))
        .route("/branding/logo", get(branding::logo))
        .route("/branding/background", get(branding::background))
        .route("/branding/manifest.webmanifest", get(branding::manifest))
        .route("/admin/branding", put(branding::update))
        .route("/login-log", get(logs::login_log))
        .route("/login-log/export", get(logs::export_login_log))
        // Errors the page ran into (anyone may report, within limits), and the error log for administrators
        .route("/client-errors", post(logs::client_report))
        .route("/admin/errors", get(logs::error_log))
        .route("/admin/errors/export", get(logs::export_errors))
        .route("/admin/logs", get(logs::get_status).put(logs::update_settings))
        .route("/admin/logs/archive", post(logs::archive_now))
        .route("/admin/logs/archives/{id}", get(logs::download_archive).delete(logs::delete_archive))
}

/// Administration: accounts and groups, settings, Storage usage, spaces and their moves, backups and replicas
fn admin_api() -> Router<AppState> {
    Router::new()
        .route("/admin/users", get(admin::list).post(admin::create))
        .route("/admin/users/{id}", patch(admin::update).delete(admin::delete))
        .route("/admin/users/{id}/sessions", get(sessions::admin_list).delete(sessions::admin_sign_out_all))
        .route("/admin/users/{id}/sessions/{session}", delete(sessions::admin_sign_out))
        .route("/admin/users/{id}/2fa", delete(twofactor::admin_reset))
        .route("/admin/users/{id}/personal-space", post(admin::personal::add).delete(admin::personal::remove))
        .route("/admin/groups", get(drives::list_groups).post(drives::create_group))
        .route("/admin/groups/{id}", patch(drives::update_group).delete(drives::delete_group))
        .route("/admin/settings", get(admin::get_settings).patch(admin::update_settings))
        .route("/admin/sso", get(sso::get_settings).put(sso::update_settings))
        .route("/admin/usage", get(usage::api::overview))
        .route("/admin/usage/history", get(usage::api::history))
        .route("/admin/usage/thresholds", put(usage::api::set_thresholds))
        .route("/admin/drives", get(drives::admin_list))
        .route("/admin/drives/{id}/scan", post(drives::scan))
        .route("/admin/moves", get(moves::list).post(moves::create))
        .route("/admin/moves/settings", axum::routing::put(moves::update_settings))
        .route("/admin/moves/{id}/pause", post(moves::pause))
        .route("/admin/moves/{id}/resume", post(moves::resume))
        .route("/admin/moves/{id}/cancel", post(moves::cancel))
        .route("/admin/backups", get(backups::api::list))
        .route("/admin/backups/copies", post(backups::api::copy))
        .route("/admin/backups/copies/preview", post(backups::api::copy_preview))
        .route("/admin/backups/import", post(backups::api::import))
        .route("/admin/backups/policies", post(backups::api::create_policy))
        .route("/admin/backups/policies/next-runs", post(backups::api::next_runs))
        .route("/admin/backups/policies/{id}", patch(backups::api::update_policy))
        .route("/admin/backups/policies/{id}/run", post(backups::api::run_policy))
        .route("/admin/backups/jobs/{id}/pause", post(backups::api::pause))
        .route("/admin/backups/jobs/{id}/resume", post(backups::api::resume))
        .route("/admin/backups/jobs/{id}/cancel", post(backups::api::cancel))
        .route("/admin/backups/sets/{id}", delete(backups::api::delete))
        .route("/admin/backups/sets/{id}/verify", post(backups::api::verify))
        .route("/admin/backups/snapshots/{id}/browse", get(backups::api::browse))
        .route("/admin/backups/snapshots/{id}/restore", post(backups::api::restore))
        .route("/admin/backups/snapshots/{id}/restore/preview", post(backups::api::restore_preview))
        .route("/admin/replicas", get(replicas::api::list).post(replicas::api::create))
        .route("/admin/replicas/purge", post(replicas::api::purge))
        .route("/admin/replicas/jobs/{id}/pause", post(replicas::api::pause_job))
        .route("/admin/replicas/jobs/{id}/resume", post(replicas::api::resume_job))
        .route("/admin/replicas/jobs/{id}/cancel", post(replicas::api::cancel_job))
        .route("/admin/replicas/{id}", patch(replicas::api::update).delete(replicas::api::delete))
        .route("/admin/replicas/{id}/sync", post(replicas::api::sync))
        .route("/admin/replicas/{id}/verify", post(replicas::api::verify))
        .route("/admin/replicas/{id}/promote", get(replicas::api::promote_preview).post(replicas::api::promote))
}

/// Administration of the storage locations, and what can be done with one
fn storage_api() -> Router<AppState> {
    Router::new()
        .route("/admin/storage", get(locations::list).post(locations::create))
        .route("/admin/storage/test", post(locations::test))
        .route("/admin/storage/{id}", patch(locations::update).delete(locations::delete))
        .route("/admin/storage/{id}/test", post(locations::test_existing))
        .route("/admin/storage/{id}/default", post(locations::set_default))
        .route("/admin/storage/{id}/spaces", get(locations::spaces))
        .route("/admin/storage/{id}/test-steps", post(location_tools::test_steps))
        .route("/admin/storage/{id}/browse", get(location_tools::browse))
        .route("/admin/storage/{id}/unused", get(location_tools::unused_status).post(location_tools::find_unused))
        .route("/admin/storage/{id}/unused/remove", post(location_tools::remove_unused))
}

#[cfg(test)]
mod tests {
    use axum::{
        http::{Method, header},
        response::Response,
    };
    use tower::ServiceExt;

    use super::*;
    use crate::testutil;

    async fn call(app: &Router, method: Method, uri: &str, headers: &[(header::HeaderName, String)], body: Option<serde_json::Value>) -> Response {
        let mut req =
            axum::http::Request::builder().method(method).uri(uri).extension(axum::extract::ConnectInfo(std::net::SocketAddr::from(([10, 0, 0, 1], 5000))));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let req = match body {
            Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(axum::body::Body::from(b.to_string())),
            None => req.body(axum::body::Body::empty()),
        };
        app.clone().oneshot(req.unwrap()).await.unwrap()
    }

    async fn app_password(env: &testutil::TestEnv, user: &auth::User, scope: &str) -> String {
        let (_, cookie) = env.sign_in(user, "Test").await;
        let res = call(
            &router(env.st.clone()),
            Method::POST,
            "/api/auth/app-passwords",
            &[(header::COOKIE, cookie)],
            Some(serde_json::json!({ "name": "Script", "scope": scope, "password": testutil::password() })),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["token"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn app_passwords_work_for_file_operations_only() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let app = router(env.st.clone());
        let token = app_password(&env, &admin, "write").await;
        let bearer = || vec![(header::AUTHORIZATION, format!("Bearer {token}"))];

        let res = call(&app, Method::GET, "/api/auth/me", &bearer(), None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::SET_COOKIE));
        assert_eq!(call(&app, Method::GET, "/api/drives", &bearer(), None).await.status(), StatusCode::OK);
        assert_eq!(call(&app, Method::GET, &format!("/api/nodes/{}/children", admin.root()), &bearer(), None).await.status(), StatusCode::OK);
        // Scripts send no Origin; a token request that does isn't a cross-site forgery either
        let mut with_origin = bearer();
        with_origin.push((header::ORIGIN, "https://elsewhere.example".into()));
        let folder = serde_json::json!({ "parent_id": admin.root(), "name": "From a script" });
        assert_eq!(call(&app, Method::POST, "/api/folders", &with_origin, Some(folder)).await.status(), StatusCode::OK);

        // The account, sign-in methods, sharing and administration need a browser session
        for (method, uri, body) in [
            (Method::GET, "/api/auth/sessions", None),
            (Method::GET, "/api/auth/app-passwords", None),
            (Method::POST, "/api/auth/app-passwords", Some(serde_json::json!({ "name": "x", "scope": "write" }))),
            (Method::PUT, "/api/auth/password", Some(serde_json::json!({ "current": "a", "new": "abcdefgh" }))),
            (Method::GET, "/api/auth/identities", None),
            (Method::POST, "/api/auth/sso/google/link", Some(serde_json::json!({}))),
            (Method::GET, "/api/shares", None),
            (Method::GET, "/api/admin/users", None),
            (Method::PATCH, "/api/admin/settings", Some(serde_json::json!({ "allow_user_drives": true }))),
            (Method::POST, "/api/drives", Some(serde_json::json!({ "name": "Team" }))),
        ] {
            assert_eq!(call(&app, method, uri, &bearer(), body).await.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }
    }

    #[tokio::test]
    async fn read_only_app_passwords_cant_change_files() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let app = router(env.st.clone());
        let token = app_password(&env, &amy, "read").await;
        let auth = vec![(header::AUTHORIZATION, format!("Basic {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("amy:{token}"))))];
        assert_eq!(call(&app, Method::GET, &format!("/api/nodes/{}/children", amy.root()), &auth, None).await.status(), StatusCode::OK);
        let folder = serde_json::json!({ "parent_id": amy.root(), "name": "Nope" });
        assert_eq!(call(&app, Method::POST, "/api/folders", &auth, Some(folder)).await.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn proxy_headers_count_only_from_a_trusted_proxy_and_https_sites_ask_for_https() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let app = router(env.st.clone());
        let (_, cookie) = env.sign_in(&amy, "Test").await;
        // Not from a trusted proxy (THIRTYFILE_TRUST_PROXY is off): X-Forwarded-Host can't make another site's request look
        // like this one's
        let forged = vec![
            (header::COOKIE, cookie.clone()),
            (header::ORIGIN, "https://evil.example".into()),
            (header::HeaderName::from_static("x-forwarded-host"), "evil.example".into()),
        ];
        let folder = serde_json::json!({ "parent_id": amy.root(), "name": "Forged" });
        assert_eq!(call(&app, Method::POST, "/api/folders", &forged, Some(folder)).await.status(), StatusCode::FORBIDDEN);
        assert!(call(&app, Method::GET, "/api/auth/me", &[(header::COOKIE, cookie.clone())], None).await.headers().get(header::STRICT_TRANSPORT_SECURITY).is_none());

        // An https Site URL: cookies are Secure and browsers are told to keep to HTTPS; signing out clears their cache
        env.st.system.write().public_url = "https://drive.example.com".into();
        let res = call(&app, Method::GET, "/api/auth/me", &[(header::COOKIE, cookie.clone())], None).await;
        assert_eq!(res.headers()[header::STRICT_TRANSPORT_SECURITY], "max-age=31536000");
        assert!(auth::cookie_header(&env.st, "x", "y", "/", 1).ends_with("; Secure"));
        let res = call(&app, Method::POST, "/api/auth/logout", &[(header::COOKIE, cookie)], None).await;
        assert_eq!(res.headers()["clear-site-data"], "\"cache\"");
    }

    #[tokio::test]
    async fn sessions_still_need_the_same_origin() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let app = router(env.st.clone());
        let token = app_password(&env, &amy, "write").await;
        let (_, cookie) = env.sign_in(&amy, "Test").await;
        // A cookie together with some token: the browser would add the cookie by itself, so the origin is checked
        let headers = vec![(header::COOKIE, cookie), (header::AUTHORIZATION, format!("Bearer {token}")), (header::ORIGIN, "https://elsewhere.example".into())];
        let folder = serde_json::json!({ "parent_id": amy.root(), "name": "Forged" });
        assert_eq!(call(&app, Method::POST, "/api/folders", &headers, Some(folder)).await.status(), StatusCode::FORBIDDEN);

        // Basic credentials a browser remembered from the WebDAV sign-in prompt: checked too, without a cookie
        let basic = format!("Basic {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("amy:{token}")));
        let headers = vec![(header::AUTHORIZATION, basic.clone()), (header::ORIGIN, "https://elsewhere.example".into())];
        let folder = serde_json::json!({ "parent_id": amy.root(), "name": "Forged" });
        assert_eq!(call(&app, Method::POST, "/api/folders", &headers, Some(folder.clone())).await.status(), StatusCode::FORBIDDEN);
        // A script sends no Origin, or a Bearer token from anywhere: both work
        assert_eq!(call(&app, Method::POST, "/api/folders", &[(header::AUTHORIZATION, basic)], Some(folder)).await.status(), StatusCode::OK);
        let headers = vec![(header::AUTHORIZATION, format!("Bearer {token}")), (header::ORIGIN, "https://elsewhere.example".into())];
        let folder = serde_json::json!({ "parent_id": amy.root(), "name": "From a script" });
        assert_eq!(call(&app, Method::POST, "/api/folders", &headers, Some(folder)).await.status(), StatusCode::OK);
    }
}
