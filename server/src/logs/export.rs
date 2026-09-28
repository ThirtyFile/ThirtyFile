//! Exporting the logs as CSV files, in the interface language

use axum::{
    extract::{Query, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};

use super::{ActivityQuery, DAY, LoginQuery, authorize_activity, query_activity, query_logins, scope_logins};
use crate::{
    auth::User,
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
    // File names
    ("activity-log", "活動紀錄"),
    ("share-access-log", "分享訪問紀錄"),
    ("login-log", "登入紀錄"),
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
    ("Create space", "建立空間"),
    ("Update space", "更新空間"),
    ("Delete space", "刪除空間"),
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
    ("System settings", "系統設定"),
    ("Archive logs", "封存紀錄"),
    ("Delete archive", "刪除封存檔"),
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
    let rows = query_activity(&st, &q, EXPORT_LIMIT).await?;
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
    (
        [(header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, content_disposition("attachment", &name))],
        out,
    )
        .into_response()
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
        "drive_create" => "Create space",
        "drive_update" => "Update space",
        "drive_delete" => "Delete space",
        "drive_location" => "Change storage location",
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
        "settings" => "System settings",
        "log_archive" => "Archive logs",
        "log_archive_delete" => "Delete archive",
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
