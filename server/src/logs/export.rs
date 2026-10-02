//! Exporting the logs as CSV files, in the interface language

use axum::{
    extract::{Query, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};

use super::{ActivityQuery, DAY, ErrorQuery, LoginQuery, authorize_activity, query_activity, query_errors, query_logins, scope_logins};
use crate::{
    auth::{Admin, User},
    error::AppResult,
    state::AppState,
    util::{content_disposition, now},
};

/// Row limit for CSV exports
const EXPORT_LIMIT: i64 = 100_000;

/// Whether exports should be in English: the interface language set by the frontend (`tf_lang` cookie) is English.
/// Otherwise exported files are in Traditional Chinese (see [`ZH_TW`]).
pub(super) fn english(headers: &HeaderMap) -> bool {
    crate::auth::get_cookie(headers, "tf_lang") == Some("en")
}

/// Localization data for exported files (CSV headers, action / event / method labels, file names):
/// English source text → Traditional Chinese (zh-TW). Used when the interface language isn't English.
const ZH_TW: &[(&str, &str)] = &[
    // CSV column headers
    ("Time", "時間"),
    ("User", "使用者"),
    ("Action", "動作"),
    ("Item", "項目"),
    ("Details", "細節"),
    ("Space", "空間"),
    ("Account", "帳號"),
    ("Event", "事件"),
    ("Method", "方式"),
    ("Browser", "瀏覽器"),
    ("Source", "來源"),
    ("Severity", "嚴重程度"),
    ("Kind", "類型"),
    ("Operation", "操作"),
    ("Status", "狀態碼"),
    ("Message", "訊息"),
    ("Page report", "網頁回報"),
    ("Request ID", "要求 ID"),
    ("Count", "次數"),
    ("First seen", "首次發生"),
    ("Version", "版本"),
    // Error log values
    ("Server", "伺服器"),
    ("Web page", "網頁"),
    ("Error", "錯誤"),
    ("Warning", "警告"),
    ("Not signed in", "未登入"),
    // File names
    ("activity-log", "活動紀錄"),
    ("share-access-log", "分享訪問紀錄"),
    ("login-log", "登入紀錄"),
    ("error-log", "錯誤紀錄"),
    // Activity actions
    ("Upload", "上傳"),
    ("Create folder", "建立資料夾"),
    ("Rename", "重新命名"),
    ("Move", "移動"),
    ("Copy", "複製"),
    ("Compress to ZIP", "壓縮成 ZIP"),
    ("Extract", "解壓縮"),
    ("Move to trash", "移至垃圾桶"),
    ("Restore", "還原"),
    ("Delete permanently", "永久刪除"),
    ("Empty trash", "清空垃圾桶"),
    ("Edit", "編輯"),
    ("Grant access", "授予存取權"),
    ("Remove access", "移除存取權"),
    ("Create share link", "建立分享連結"),
    ("Disable share link", "停用分享連結"),
    ("Change share link", "變更分享連結"),
    ("Create space", "建立空間"),
    ("Update space", "更新空間"),
    ("Delete space", "刪除空間"),
    ("Start moving to another location", "開始搬到其他位置"),
    ("Moved to another location", "已搬到其他位置"),
    ("Moving to another location failed", "搬到其他位置失敗"),
    ("Cancel moving to another location", "取消搬到其他位置"),
    ("Checked the folder", "檢查資料夾"),
    ("Change storage location", "變更儲存位置"),
    ("Create group", "建立群組"),
    ("Update group", "更新群組"),
    ("Delete group", "刪除群組"),
    ("Add user", "新增使用者"),
    ("Edit user", "修改使用者"),
    ("Delete user", "刪除使用者"),
    ("Add storage location", "新增儲存位置"),
    ("Edit storage location", "修改儲存位置"),
    ("Delete storage location", "刪除儲存位置"),
    ("Set default storage location", "設定預設儲存位置"),
    ("Remove unused content", "移除未使用的內容"),
    ("System settings", "系統設定"),
    ("Archive logs", "封存紀錄"),
    ("Delete archive", "刪除封存檔"),
    ("Start copying a location", "開始製作位置的副本"),
    ("Snapshot made", "快照已完成"),
    ("Backup job failed", "備份工作失敗"),
    ("Cancel backup job", "取消備份工作"),
    ("Start restoring from a backup", "開始從備份還原"),
    ("Restored from a backup", "已從備份還原"),
    ("Backup checked", "備份已檢查"),
    ("Start deleting a backup", "開始刪除備份"),
    ("Backup deleted", "備份已刪除"),
    ("Create backup policy", "建立備份規則"),
    ("Change backup policy", "變更備份規則"),
    ("Back up now", "立即備份"),
    ("Backup needs attention", "備份需要處理"),
    ("Found backups on a location", "在位置上找到備份"),
    ("Create replica policy", "建立鏡像規則"),
    ("Change replica policy", "變更鏡像規則"),
    ("Delete replica policy", "刪除鏡像規則"),
    ("Replica sync failed", "鏡像同步失敗"),
    ("Replicas need attention", "鏡像需要處理"),
    ("Read from a replica", "從鏡像讀取"),
    ("Promote a replica", "提升鏡像"),
    ("Remove copies no policy wants", "移除沒有規則需要的鏡像"),
    // Sign-in events and methods
    ("Signed in", "登入成功"),
    ("Wrong password", "密碼錯誤"),
    ("Unknown account", "帳號不存在"),
    ("Account disabled", "帳號已停用"),
    ("Temporarily locked", "暫時鎖定"),
    ("Signed out", "登出"),
    ("Password changed", "變更密碼"),
    ("Password reset asked for", "要求重設密碼"),
    ("Password reset by email", "透過郵件重設密碼"),
    ("SSO sign-in denied", "三方登入被拒"),
    ("Account created by SSO", "三方登入自動建立帳號"),
    ("External account linked", "連結外部帳號"),
    ("External account unlinked", "取消連結外部帳號"),
    ("Device signed out", "登出裝置"),
    ("Signed out on other devices", "登出其他裝置"),
    ("Signed out by an administrator", "由管理員登出"),
    ("Wrong app password", "應用程式密碼錯誤"),
    ("App password created", "建立應用程式密碼"),
    ("App password removed", "移除應用程式密碼"),
    ("App password", "應用程式密碼"),
    ("Wrong two-factor code", "兩步驟驗證碼錯誤"),
    ("Two-factor sign-in turned on", "開啟兩步驟驗證"),
    ("Two-factor sign-in turned off", "關閉兩步驟驗證"),
    ("Two-factor sign-in reset by an administrator", "管理員重設兩步驟驗證"),
    ("Recovery code used", "使用復原碼"),
    ("New recovery codes", "產生新的復原碼"),
    ("Password", "帳號密碼"),
];

/// Localizes export text: English is returned as is; otherwise the zh-TW rendering (text missing from the table stays English)
pub(super) fn localize(s: &str, en: bool) -> &str {
    if en {
        return s;
    }
    ZH_TW.iter().find(|(k, _)| *k == s).map_or(s, |&(_, v)| v)
}

/// Exports matching activity log entries (CSV with a BOM, so Excel opens non-ASCII text correctly)
pub async fn export_activity(State(st): State<AppState>, user: User, headers: HeaderMap, Query(q): Query<ActivityQuery>) -> AppResult<Response> {
    let en = english(&headers);
    authorize_activity(&st, &user, &q).await?;
    let rows = query_activity(&st, &q, user.id, EXPORT_LIMIT).await?;
    Ok(csv_file(en, q.tz, "activity-log", &["Time", "User", "Action", "Item", "Details", "Space"], rows, |r, offset| {
        let action = localize(action_label(&r.action), en).to_string();
        vec![format_time(r.at, offset), r.username, action, r.node_name, r.detail, r.drive_name.unwrap_or_default()]
    }))
}

/// A CSV file (with a BOM, so Excel opens non-ASCII text correctly) named `<name>-<date>.csv`, with localized column
/// names and one line per record; `line` gets each record and the client's UTC offset in seconds
fn csv_file<T>(en: bool, tz: Option<i64>, name: &str, columns: &[&str], rows: Vec<T>, line: impl Fn(T, i64) -> Vec<String>) -> Response {
    let offset = -tz_minutes(tz) * 60;
    let mut out = format!("\u{feff}{}\n", columns.iter().map(|c| localize(c, en)).collect::<Vec<_>>().join(","));
    for r in rows {
        out.push_str(&line(r, offset).iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    let name = format!("{}-{}.csv", localize(name, en), format_time(now(), offset).get(..10).unwrap_or("").replace('-', ""));
    ([(header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, content_disposition("attachment", &name))], out).into_response()
}

/// Action names in exports (matching the names shown in the web interface)
fn action_label(a: &str) -> &str {
    match a {
        "upload" => "Upload",
        "create_folder" => "Create folder",
        "rename" => "Rename",
        "move" => "Move",
        "copy" => "Copy",
        "compress" => "Compress to ZIP",
        "extract" => "Extract",
        "trash" => "Move to trash",
        "restore" => "Restore",
        "delete" => "Delete permanently",
        "empty_trash" => "Empty trash",
        "edit" => "Edit",
        "grant" => "Grant access",
        "revoke" => "Remove access",
        "share_create" => "Create share link",
        "share_delete" => "Disable share link",
        "share_update" => "Change share link",
        "drive_create" => "Create space",
        "drive_update" => "Update space",
        "drive_delete" => "Delete space",
        "move_start" => "Start moving to another location",
        "move_done" => "Moved to another location",
        "move_failed" => "Moving to another location failed",
        "move_cancel" => "Cancel moving to another location",
        "scan" => "Checked the folder",
        "group_create" => "Create group",
        "group_update" => "Update group",
        "group_delete" => "Delete group",
        "user_create" => "Add user",
        "user_update" => "Edit user",
        "user_delete" => "Delete user",
        "storage_create" => "Add storage location",
        "storage_update" => "Edit storage location",
        "storage_delete" => "Delete storage location",
        "storage_default" => "Set default storage location",
        "storage_cleanup" => "Remove unused content",
        "settings" => "System settings",
        "log_archive" => "Archive logs",
        "log_archive_delete" => "Delete archive",
        "backup_copy" => "Start copying a location",
        "backup_done" => "Snapshot made",
        "backup_failed" => "Backup job failed",
        "backup_cancel" => "Cancel backup job",
        "backup_restore" => "Start restoring from a backup",
        "backup_restored" => "Restored from a backup",
        "backup_verified" => "Backup checked",
        "backup_delete_start" => "Start deleting a backup",
        "backup_delete" => "Backup deleted",
        "backup_policy_create" => "Create backup policy",
        "backup_policy_update" => "Change backup policy",
        "backup_run" => "Back up now",
        "backup_alert" => "Backup needs attention",
        "backup_import" => "Found backups on a location",
        "replica_create" => "Create replica policy",
        "replica_update" => "Change replica policy",
        "replica_delete" => "Delete replica policy",
        "replica_failed" => "Replica sync failed",
        "replica_alert" => "Replicas need attention",
        "replica_read" => "Read from a replica",
        "replica_promote" => "Promote a replica",
        "replica_purge" => "Remove copies no policy wants",
        other => other,
    }
}

pub(super) fn csv_field(s: &str) -> String {
    // Content starting with = + - @ (or a tab or carriage return before one) may be run as a formula in spreadsheets,
    // so prefix it with a single quote
    let safe = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{s}") } else { s.to_string() };
    if safe.contains([',', '"', '\n', '\r']) { format!("\"{}\"", safe.replace('"', "\"\"")) } else { safe }
}

/// The client's UTC offset in minutes, kept within the real range so the arithmetic on timestamps can't overflow
fn tz_minutes(tz: Option<i64>) -> i64 {
    tz.unwrap_or(0).clamp(-14 * 60, 14 * 60)
}

/// Unix seconds + time zone offset → YYYY-MM-DD HH:MM:SS
pub fn format_time(ts: i64, offset: i64) -> String {
    let t = ts + offset;
    let (days, secs) = (t.div_euclid(DAY), t.rem_euclid(DAY));
    let (y, m, d) = crate::util::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// Exports matching error log entries, for administrators
pub async fn export_errors(State(st): State<AppState>, _: Admin, headers: HeaderMap, Query(q): Query<ErrorQuery>) -> AppResult<Response> {
    let en = english(&headers);
    let rows = query_errors(&st, &q, EXPORT_LIMIT).await?;
    let columns =
        ["Time", "First seen", "Count", "User", "Source", "Severity", "Kind", "Operation", "Status", "Message", "Details", "Page report", "Request ID", "Version"];
    Ok(csv_file(en, q.tz, "error-log", &columns, rows, |r, offset| {
        let source = localize(if r.source == "backend" { "Server" } else { "Web page" }, en);
        let severity = localize(if r.severity == "error" { "Error" } else { "Warning" }, en);
        let user = if r.user_id.is_none() && r.username.is_empty() { localize("Not signed in", en).to_string() } else { r.username };
        vec![
            format_time(r.at, offset),
            format_time(r.first_at, offset),
            r.count.to_string(),
            user,
            source.to_string(),
            severity.to_string(),
            r.kind,
            r.operation,
            r.status.map(|s| s.to_string()).unwrap_or_default(),
            r.message,
            r.detail,
            r.client,
            r.request_id.unwrap_or_default(),
            r.version,
        ]
    }))
}

pub async fn export_login_log(State(st): State<AppState>, user: User, headers: HeaderMap, Query(mut q): Query<LoginQuery>) -> AppResult<Response> {
    let en = english(&headers);
    scope_logins(&user, &mut q);
    let rows = query_logins(&st, &q, EXPORT_LIMIT).await?;
    Ok(csv_file(en, q.tz, "login-log", &["Time", "Account", "Event", "Method", "IP", "Browser"], rows, |r, offset| {
        let (event, method) = (localize(login_event_label(&r.event), en), localize(login_method_label(&r.method), en));
        vec![format_time(r.at, offset), r.username, event.to_string(), method.to_string(), r.ip, r.user_agent]
    }))
}

fn login_method_label(m: &str) -> &str {
    match m {
        "password" => "Password",
        "microsoft" => "Microsoft",
        "google" => "Google",
        "github" => "GitHub",
        "app_password" => "App password",
        other => other,
    }
}

fn login_event_label(e: &str) -> &str {
    match e {
        "login" => "Signed in",
        "bad_password" => "Wrong password",
        "unknown_user" => "Unknown account",
        "disabled" => "Account disabled",
        "locked" => "Temporarily locked",
        "logout" => "Signed out",
        "password_change" => "Password changed",
        "password_reset_requested" => "Password reset asked for",
        "password_reset" => "Password reset by email",
        "sso_denied" => "SSO sign-in denied",
        "sso_provisioned" => "Account created by SSO",
        "sso_link" => "External account linked",
        "sso_unlink" => "External account unlinked",
        "device_signout" => "Device signed out",
        "signout_others" => "Signed out on other devices",
        "admin_signout" => "Signed out by an administrator",
        "app_password_failed" => "Wrong app password",
        "app_password_created" => "App password created",
        "app_password_revoked" => "App password removed",
        "2fa_failed" => "Wrong two-factor code",
        "2fa_enabled" => "Two-factor sign-in turned on",
        "2fa_disabled" => "Two-factor sign-in turned off",
        "2fa_reset" => "Two-factor sign-in reset by an administrator",
        "recovery_code_used" => "Recovery code used",
        "recovery_codes_new" => "New recovery codes",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderValue, StatusCode};

    use super::*;
    use crate::{auth::Admin, testutil};

    /// Every action the activity log records (the web page names the same ones, components/logs/actions.ts)
    const ACTIONS: &[&str] = &[
        "upload",
        "create_folder",
        "rename",
        "move",
        "copy",
        "compress",
        "extract",
        "trash",
        "restore",
        "delete",
        "empty_trash",
        "edit",
        "grant",
        "revoke",
        "share_create",
        "share_update",
        "share_delete",
        "scan",
        "drive_create",
        "drive_update",
        "drive_delete",
        "move_start",
        "move_done",
        "move_failed",
        "move_cancel",
        "group_create",
        "group_update",
        "group_delete",
        "user_create",
        "user_update",
        "user_delete",
        "storage_create",
        "storage_update",
        "storage_delete",
        "storage_default",
        "storage_cleanup",
        "settings",
        "log_archive",
        "log_archive_delete",
        "backup_copy",
        "backup_done",
        "backup_failed",
        "backup_cancel",
        "backup_restore",
        "backup_restored",
        "backup_verified",
        "backup_delete_start",
        "backup_delete",
        "backup_policy_create",
        "backup_policy_update",
        "backup_run",
        "backup_alert",
        "backup_import",
        "replica_create",
        "replica_update",
        "replica_delete",
        "replica_failed",
        "replica_alert",
        "replica_read",
        "replica_promote",
        "replica_purge",
    ];

    const SIGN_IN_EVENTS: &[&str] = &[
        "login",
        "bad_password",
        "unknown_user",
        "disabled",
        "locked",
        "logout",
        "password_change",
        "password_reset_requested",
        "password_reset",
        "sso_denied",
        "sso_provisioned",
        "sso_link",
        "sso_unlink",
        "device_signout",
        "signout_others",
        "admin_signout",
        "app_password_failed",
        "app_password_created",
        "app_password_revoked",
        "2fa_failed",
        "2fa_enabled",
        "2fa_disabled",
        "2fa_reset",
        "recovery_code_used",
        "recovery_codes_new",
    ];

    #[test]
    fn every_action_and_sign_in_event_is_named_in_both_languages() {
        for (code, en) in ACTIONS.iter().map(|a| (*a, action_label(a))).chain(SIGN_IN_EVENTS.iter().map(|e| (*e, login_event_label(e)))) {
            assert_ne!(en, code, "{code} has no name in exports");
            assert_ne!(localize(en, false), en, "\"{en}\" has no Traditional Chinese");
            assert_eq!(localize(en, true), en);
        }
        for method in ["password", "app_password"] {
            assert_ne!(localize(login_method_label(method), false), login_method_label(method));
        }
        // Names of other companies' services stay as they are
        assert_eq!(localize(login_method_label("github"), false), "GitHub");
        // Something new is exported as it was recorded rather than left out
        assert_eq!(action_label("something_new"), "something_new");
    }

    /// An exported file: its name (as the browser saves it, decoded) and its text
    async fn exported(res: AppResult<Response>) -> (String, String) {
        let res = res.unwrap();
        assert_eq!(res.headers()[header::CONTENT_TYPE], "text/csv; charset=utf-8");
        let disposition = res.headers()[header::CONTENT_DISPOSITION].to_str().unwrap().to_string();
        let encoded = disposition.split("filename*=UTF-8''").nth(1).unwrap();
        let name = percent_encoding::percent_decode_str(encoded).decode_utf8().unwrap().into_owned();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (name, String::from_utf8(body.to_vec()).unwrap())
    }

    fn english_cookie() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("theme=dark; tf_lang=en"));
        h
    }

    #[tokio::test]
    async fn the_activity_log_exports_in_the_interface_language_with_local_times_and_cells_spreadsheets_wont_run() {
        let env = testutil::env().await;
        let (admin, amy) = (env.admin().await, env.user("amy", true).await);
        // 2026-09-25 13:10:30 UTC
        let at = 1_790_341_830;
        let insert = |action: &'static str, name: &'static str, detail: &'static str, private_to: Option<i64>| {
            sqlx::query("INSERT INTO activity (at, user_id, username, node_name, action, detail, private_to) VALUES (?, ?, 'amy', ?, ?, ?, ?)")
                .bind(at)
                .bind(amy.id)
                .bind(name)
                .bind(action)
                .bind(detail)
                .bind(private_to)
                .execute(&env.st.db)
        };
        insert("upload", "=SUM(A1:A9)", "from a, b", None).await.unwrap();
        insert("backup_run", "", "", None).await.unwrap();
        insert("rename", "salary.xlsx", "secret detail", Some(amy.id)).await.unwrap();

        // English, in the person's time zone (UTC+8: JavaScript's offset is -480)
        let q = ActivityQuery { tz: Some(-480), ..Default::default() };
        let (name, text) = exported(export_activity(State(env.st.clone()), admin.clone(), english_cookie(), Query(q)).await).await;
        assert!(name.starts_with("activity-log-") && name.ends_with(".csv"), "{name}");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "\u{feff}Time,User,Action,Item,Details,Space", "a BOM first, so Excel reads it as UTF-8");
        assert!(lines.contains(&"2026-09-25 21:10:30,amy,Upload,'=SUM(A1:A9),\"from a, b\","), "{text}");
        assert!(lines.contains(&"2026-09-25 21:10:30,amy,Back up now,,,"), "{text}");
        // An entry about someone else's personal space says who did what, not to what
        assert!(lines.contains(&"2026-09-25 21:10:30,amy,Rename,,,"), "{text}");
        assert!(!text.contains("salary") && !text.contains("secret"));

        // Otherwise in Traditional Chinese, and a time zone off the map is kept on it
        let q = ActivityQuery { tz: Some(100_000), action: Some("upload".into()), ..Default::default() };
        let (name, text) = exported(export_activity(State(env.st.clone()), admin, HeaderMap::new(), Query(q)).await).await;
        assert!(name.starts_with("活動紀錄-"), "{name}");
        assert_eq!(text.lines().next().unwrap(), "\u{feff}時間,使用者,動作,項目,細節,空間");
        // UTC-14 at most
        assert_eq!(text.lines().nth(1).unwrap(), "2026-09-24 23:10:30,amy,上傳,'=SUM(A1:A9),\"from a, b\",");

        // Only administrators export the whole log
        let err = export_activity(State(env.st.clone()), amy, HeaderMap::new(), Query(ActivityQuery::default())).await.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn sign_ins_export_for_their_person_and_errors_for_administrators() {
        let env = testutil::env().await;
        let (admin, amy) = (env.admin().await, env.user("amy", true).await);
        for (user_id, username, event, method) in
            [(Some(amy.id), "amy", "login", "google"), (Some(admin.id), "admin", "bad_password", "password"), (None, "nobody", "unknown_user", "password")]
        {
            sqlx::query("INSERT INTO login_log (at, user_id, username, event, ip, user_agent, method) VALUES (0, ?, ?, ?, '10.0.0.1', 'Firefox, on Linux', ?)")
                .bind(user_id)
                .bind(username)
                .bind(event)
                .bind(method)
                .execute(&env.st.db)
                .await
                .unwrap();
        }
        let (name, text) = exported(export_login_log(State(env.st.clone()), admin.clone(), english_cookie(), Query(LoginQuery::default())).await).await;
        assert!(name.starts_with("login-log-"));
        assert_eq!(text.lines().next().unwrap(), "\u{feff}Time,Account,Event,Method,IP,Browser");
        assert!(text.contains("1970-01-01 00:00:00,amy,Signed in,Google,10.0.0.1,\"Firefox, on Linux\""), "{text}");
        assert!(text.contains(",nobody,Unknown account,Password,"), "{text}");
        // Someone else asking gets their own sign-ins only, whatever they filter by
        let q = LoginQuery { user: Some("admin".into()), ..Default::default() };
        let (name, text) = exported(export_login_log(State(env.st.clone()), amy, HeaderMap::new(), Query(q)).await).await;
        assert!(name.starts_with("登入紀錄-"));
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(text.contains(",amy,登入成功,Google,"), "{text}");

        sqlx::query(
            "INSERT INTO error_log (at, first_at, count, source, severity, kind, user_id, username, operation, status, message, detail, request_id, client, version, fingerprint)
             VALUES (60, 0, 3, 'backend', 'error', 'storage', NULL, '', 'GET /api/files/{id}/content', 503, 'Storage unavailable', '+cmd', 'r-1', '', '1.2.3', 'f')",
        )
        .execute(&env.st.db)
        .await
        .unwrap();
        let (name, text) = exported(export_errors(State(env.st.clone()), Admin(admin), HeaderMap::new(), Query(ErrorQuery::default())).await).await;
        assert!(name.starts_with("錯誤紀錄-"));
        assert_eq!(text.lines().next().unwrap(), "\u{feff}時間,首次發生,次數,使用者,來源,嚴重程度,類型,操作,狀態碼,訊息,細節,網頁回報,要求 ID,版本");
        assert_eq!(
            text.lines().nth(1).unwrap(),
            "1970-01-01 00:01:00,1970-01-01 00:00:00,3,未登入,伺服器,錯誤,storage,GET /api/files/{id}/content,503,Storage unavailable,'+cmd,,r-1,1.2.3"
        );
    }
}
