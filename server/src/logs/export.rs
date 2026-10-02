//! Exporting the logs: the matching records, up to a limit, as recorded (action and event codes, Unix times). The page
//! turns them into a CSV file in its own language, with the same names it shows (web/src/lib/csv.ts), so the names live
//! in one place: the page's dictionaries.

use axum::{
    Json,
    extract::{Query, State},
};

use super::{DAY, ErrorQuery, ErrorRow, LoginQuery, LoginRow, query_errors, query_logins, scope_logins};
use crate::{
    auth::{Admin, User},
    error::AppResult,
    state::AppState,
};

/// Row limit for exports
pub(crate) const EXPORT_LIMIT: i64 = 100_000;

/// Unix seconds + time zone offset → YYYY-MM-DD HH:MM:SS
pub fn format_time(ts: i64, offset: i64) -> String {
    let t = ts + offset;
    let (days, secs) = (t.div_euclid(DAY), t.rem_euclid(DAY));
    let (y, m, d) = crate::util::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// The error log entries to export, for administrators
pub async fn export_errors(State(st): State<AppState>, _: Admin, Query(q): Query<ErrorQuery>) -> AppResult<Json<Vec<ErrorRow>>> {
    Ok(Json(query_errors(&st, &q, EXPORT_LIMIT).await?))
}

/// The sign-ins to export: someone who isn't an administrator gets their own only
pub async fn export_login_log(State(st): State<AppState>, user: User, Query(mut q): Query<LoginQuery>) -> AppResult<Json<Vec<LoginRow>>> {
    scope_logins(&user, &mut q);
    Ok(Json(query_logins(&st, &q, EXPORT_LIMIT).await?))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::Value;

    use super::*;
    use crate::{auth::Admin, history::export_activity, logs::ActivityQuery, testutil};

    fn json<T: serde::Serialize>(rows: Json<Vec<T>>) -> Vec<Value> {
        rows.0.iter().map(|r| serde_json::to_value(r).unwrap()).collect()
    }

    #[test]
    fn times_are_written_in_the_given_offset() {
        assert_eq!(format_time(0, 0), "1970-01-01 00:00:00");
        assert_eq!(format_time(1_790_341_830, 8 * 3600), "2026-09-25 21:10:30");
        assert_eq!(format_time(951_782_400, 0), "2000-02-29 00:00:00");
    }

    #[tokio::test]
    async fn the_activity_log_exports_codes_and_times_as_recorded_and_leaves_out_other_peoples_personal_items() {
        let env = testutil::env().await;
        let (admin, amy) = (env.admin().await, env.user("amy", true).await);
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

        let rows = json(export_activity(State(env.st.clone()), admin.clone(), Query(ActivityQuery::default())).await.unwrap());
        assert_eq!(rows.len(), 3);
        let upload = rows.iter().find(|r| r["action"] == "upload").unwrap();
        assert_eq!((upload["at"].as_i64(), upload["node_name"].as_str(), upload["detail"].as_str()), (Some(at), Some("=SUM(A1:A9)"), Some("from a, b")));
        assert!(rows.iter().any(|r| r["action"] == "backup_run"));
        // An entry about someone else's personal space says who did what, not to what
        let private = rows.iter().find(|r| r["action"] == "rename").unwrap();
        assert!(private["private"] == true && private["node_name"] == "" && private["detail"] == "", "{private}");

        // Filters apply
        let q = ActivityQuery { action: Some("upload".into()), ..Default::default() };
        assert_eq!(json(export_activity(State(env.st.clone()), admin, Query(q)).await.unwrap()).len(), 1);

        // Only administrators export the whole log
        let err = export_activity(State(env.st.clone()), amy, Query(ActivityQuery::default())).await.unwrap_err();
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
        let rows = json(export_login_log(State(env.st.clone()), admin.clone(), Query(LoginQuery::default())).await.unwrap());
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().any(|r| r["username"] == "amy" && r["event"] == "login" && r["method"] == "google" && r["user_agent"] == "Firefox, on Linux"));
        // Someone else asking gets their own sign-ins only, whatever they filter by
        let q = LoginQuery { user: Some("admin".into()), ..Default::default() };
        let rows = json(export_login_log(State(env.st.clone()), amy, Query(q)).await.unwrap());
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0]["username"].as_str(), rows[0]["event"].as_str()), (Some("amy"), Some("login")));

        sqlx::query(
            "INSERT INTO error_log (at, first_at, count, source, severity, kind, user_id, username, operation, status, message, detail, request_id, client, version, fingerprint)
             VALUES (60, 0, 3, 'backend', 'error', 'storage', NULL, '', 'GET /api/files/{id}/content', 503, 'Storage unavailable', '+cmd', 'r-1', '', '1.2.3', 'f')",
        )
        .execute(&env.st.db)
        .await
        .unwrap();
        let rows = json(export_errors(State(env.st.clone()), Admin(admin), Query(ErrorQuery::default())).await.unwrap());
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0]["source"].as_str(), rows[0]["severity"].as_str(), rows[0]["count"].as_i64()), (Some("backend"), Some("error"), Some(3)));
    }
}
