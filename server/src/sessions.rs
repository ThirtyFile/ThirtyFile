//! Signed-in devices: every sign-in session of an account, listed under "My account › Devices" (and per user in
//! Control panel › Users), where each one can be signed out on its own or all at once.

use std::net::SocketAddr;

use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::HeaderMap,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    auth::{Admin, User, client_ip},
    error::{AppError, AppResult},
    logs,
    state::AppState,
    tree,
    util::now,
};

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Device {
    id: String,
    user_agent: String,
    /// The address the device used most recently
    ip: String,
    /// password, microsoft, google or github
    method: String,
    created_at: i64,
    last_used_at: Option<i64>,
    /// The device this request came from
    #[sqlx(skip)]
    current: bool,
}

async fn devices_of(st: &AppState, user_id: i64, current: Option<&str>) -> AppResult<Vec<Device>> {
    let mut list: Vec<Device> = sqlx::query_as(
        "SELECT id, user_agent, ip, method, created_at, last_used_at FROM sessions
         WHERE user_id = ? AND expires_at > ? ORDER BY COALESCE(last_used_at, created_at) DESC, created_at DESC",
    )
    .bind(user_id)
    .bind(now())
    .fetch_all(&st.db)
    .await?;
    for d in &mut list {
        d.current = current == Some(d.id.as_str());
    }
    Ok(list)
}

/// Signs out one session of the user; returns whether there was one
async fn end_session(st: &AppState, user_id: i64, id: &str) -> AppResult<bool> {
    let _w = st.write_lock.lock().await;
    let res = sqlx::query("DELETE FROM sessions WHERE user_id = ? AND id = ?").bind(user_id).bind(id).execute(&st.db).await?;
    Ok(res.rows_affected() > 0)
}

/// Signs out every session of the user except `keep`; returns how many were signed out
async fn end_sessions(st: &AppState, user_id: i64, keep: Option<&str>) -> AppResult<u64> {
    let _w = st.write_lock.lock().await;
    let res = sqlx::query("DELETE FROM sessions WHERE user_id = ? AND id IS NOT ?").bind(user_id).bind(keep).execute(&st.db).await?;
    Ok(res.rows_affected())
}

// ───────────── My devices ─────────────

pub async fn list(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<Device>>> {
    Ok(Json(devices_of(&st, user.id, user.session_id.as_deref()).await?))
}

/// Signs out one of my devices (this one too, like signing out)
pub async fn sign_out(
    State(st): State<AppState>,
    user: User,
    Path(id): Path<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    if !end_session(&st, user.id, &id).await? {
        return Err(AppError::not_found("This device is already signed out"));
    }
    logs::record_login(&st, Some(user.id), &user.username, "device_signout", &client_ip(&st, addr, &headers), &headers);
    Ok(Json(json!({ "ok": true })))
}

/// Signs out all my devices except this one
pub async fn sign_out_others(
    State(st): State<AppState>,
    user: User,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let removed = end_sessions(&st, user.id, user.session_id.as_deref()).await?;
    if removed > 0 {
        logs::record_login(&st, Some(user.id), &user.username, "signout_others", &client_ip(&st, addr, &headers), &headers);
    }
    Ok(Json(json!({ "removed": removed })))
}

// ───────────── Administration: a user's devices ─────────────

async fn username_of(st: &AppState, id: i64) -> AppResult<String> {
    let row: Option<(String,)> = sqlx::query_as("SELECT username FROM users WHERE id = ?").bind(id).fetch_optional(&st.db).await?;
    row.map(|(u,)| u).ok_or_else(|| AppError::not_found("User not found"))
}

pub async fn admin_list(State(st): State<AppState>, Admin(me): Admin, Path(user_id): Path<i64>) -> AppResult<Json<Vec<Device>>> {
    username_of(&st, user_id).await?;
    // The administrator's own list marks the device they are using
    let current = if user_id == me.id { me.session_id.as_deref() } else { None };
    Ok(Json(devices_of(&st, user_id, current).await?))
}

/// Records an administrator signing out a user's devices: in the activity log, and in the user's sign-in log
async fn log_admin_sign_out(st: &AppState, me: &User, user_id: i64, username: &str, detail: &str, addr: SocketAddr, headers: &HeaderMap) -> AppResult<()> {
    {
        let _w = st.write_lock.lock().await;
        let mut conn = st.db.acquire().await?;
        tree::log(&mut conn, me, None, "user_update", &format!("{username}: {detail}")).await?;
    }
    logs::record_login(st, Some(user_id), username, "admin_signout", &client_ip(st, addr, headers), headers);
    Ok(())
}

pub async fn admin_sign_out(
    State(st): State<AppState>,
    Admin(me): Admin,
    Path((user_id, id)): Path<(i64, String)>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let username = username_of(&st, user_id).await?;
    if !end_session(&st, user_id, &id).await? {
        return Err(AppError::not_found("This device is already signed out"));
    }
    log_admin_sign_out(&st, &me, user_id, &username, "signed out one device", addr, &headers).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Signs out every device of a user (an administrator's own current device is kept)
pub async fn admin_sign_out_all(
    State(st): State<AppState>,
    Admin(me): Admin,
    Path(user_id): Path<i64>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let username = username_of(&st, user_id).await?;
    let keep = if user_id == me.id { me.session_id.as_deref() } else { None };
    let removed = end_sessions(&st, user_id, keep).await?;
    if removed > 0 {
        log_admin_sign_out(&st, &me, user_id, &username, "signed out on all devices", addr, &headers).await?;
    }
    Ok(Json(json!({ "removed": removed })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo("203.0.113.9:5000".parse().unwrap())
    }

    #[tokio::test]
    async fn devices_are_listed_and_signed_out() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (phone, phone_cookie) = env.sign_in(&amy, "Mozilla/5.0 (iPhone) Safari/605").await;
        let (laptop, laptop_cookie) = env.sign_in(&amy, "Mozilla/5.0 (Windows NT 10.0) Chrome/130").await;
        let (_, tablet_cookie) = env.sign_in(&amy, "Mozilla/5.0 (Android) Chrome/130").await;

        let Json(list) = super::list(State(env.st.clone()), laptop.clone()).await.unwrap();
        assert_eq!(list.len(), 3);
        let current: Vec<_> = list.iter().filter(|d| d.current).collect();
        assert_eq!(current.len(), 1);
        assert!(current[0].user_agent.contains("Windows") && current[0].ip == "10.0.0.1" && current[0].method == "password");

        // Signing out the phone from the laptop: the phone's cookie stops working
        let phone_id = phone.session_id.clone().unwrap();
        let _ = sign_out(State(env.st.clone()), laptop.clone(), Path(phone_id.clone()), addr(), HeaderMap::new()).await.unwrap();
        assert!(env.session_user(&phone_cookie).await.is_none());
        assert!(env.session_user(&laptop_cookie).await.is_some());
        assert_eq!(sign_out(State(env.st.clone()), laptop.clone(), Path(phone_id), addr(), HeaderMap::new()).await.unwrap_err().status, axum::http::StatusCode::NOT_FOUND);

        // Everywhere else: only the laptop stays signed in
        let Json(v) = sign_out_others(State(env.st.clone()), laptop.clone(), addr(), HeaderMap::new()).await.unwrap();
        assert_eq!(v["removed"], 1);
        assert!(env.session_user(&tablet_cookie).await.is_none());
        assert!(env.session_user(&laptop_cookie).await.is_some());
    }

    #[tokio::test]
    async fn last_use_is_recorded_at_most_every_few_minutes() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (session, cookie) = env.sign_in(&amy, "A").await;
        let id = session.session_id.unwrap();
        let last_used = || async {
            let (t, ip): (i64, String) = sqlx::query_as("SELECT last_used_at, ip FROM sessions WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
            (t, ip)
        };
        // Used a minute ago: nothing is written
        sqlx::query("UPDATE sessions SET last_used_at = last_used_at - 60 WHERE id = ?").bind(&id).execute(&env.st.db).await.unwrap();
        let before = last_used().await.0;
        let req = || {
            let mut r = axum::http::Request::builder().header(axum::http::header::COOKIE, &cookie);
            r.extensions_mut().unwrap().insert(ConnectInfo("198.51.100.4:4000".parse::<SocketAddr>().unwrap()));
            r
        };
        assert!(env.request_user(req()).await.is_some());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(last_used().await, (before, "10.0.0.1".to_string()));
        // Ten minutes ago: the time and the address are updated
        sqlx::query("UPDATE sessions SET last_used_at = last_used_at - 600 WHERE id = ?").bind(&id).execute(&env.st.db).await.unwrap();
        assert!(env.request_user(req()).await.is_some());
        for _ in 0..50 {
            if last_used().await.0 >= now() - 5 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (t, ip) = last_used().await;
        assert!(t >= now() - 5 && ip == "198.51.100.4", "{t} {ip}");
    }

    #[tokio::test]
    async fn people_can_only_sign_out_their_own_devices() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let bob = env.user("bob", true).await;
        let (amy_session, amy_cookie) = env.sign_in(&amy, "A").await;
        let (bob_session, _) = env.sign_in(&bob, "B").await;
        let amy_id = amy_session.session_id.clone().unwrap();
        let err = sign_out(State(env.st.clone()), bob_session.clone(), Path(amy_id), addr(), HeaderMap::new()).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);
        let _ = sign_out_others(State(env.st.clone()), bob_session, addr(), HeaderMap::new()).await.unwrap();
        assert!(env.session_user(&amy_cookie).await.is_some());
    }

    #[tokio::test]
    async fn administrators_see_and_sign_out_a_users_devices() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let amy = env.user("amy", true).await;
        let (amy_session, amy_cookie) = env.sign_in(&amy, "A").await;
        let (_, other_cookie) = env.sign_in(&amy, "B").await;

        let Json(list) = admin_list(State(env.st.clone()), Admin(admin.clone()), Path(amy.id)).await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|d| !d.current));
        let id = amy_session.session_id.clone().unwrap();
        let _ = admin_sign_out(State(env.st.clone()), Admin(admin.clone()), Path((amy.id, id)), addr(), HeaderMap::new()).await.unwrap();
        assert!(env.session_user(&amy_cookie).await.is_none());
        // All of them; an administrator's own current device is kept when they sign out "everywhere"
        let (admin_session, admin_cookie) = env.sign_in(&admin, "C").await;
        let (_, admin_other) = env.sign_in(&admin, "D").await;
        let Json(v) = admin_sign_out_all(State(env.st.clone()), Admin(admin_session.clone()), Path(amy.id), addr(), HeaderMap::new()).await.unwrap();
        assert_eq!(v["removed"], 1);
        assert!(env.session_user(&other_cookie).await.is_none());
        let _ = admin_sign_out_all(State(env.st.clone()), Admin(admin_session), Path(admin.id), addr(), HeaderMap::new()).await.unwrap();
        assert!(env.session_user(&admin_cookie).await.is_some() && env.session_user(&admin_other).await.is_none());

        let (details,): (String,) = sqlx::query_as("SELECT group_concat(detail, '|') FROM activity WHERE action = 'user_update'").fetch_one(&env.st.db).await.unwrap();
        assert!(details.contains("amy: signed out one device") && details.contains("amy: signed out on all devices"), "{details}");
        assert!(admin_list(State(env.st.clone()), Admin(admin), Path(9999)).await.is_err());
    }
}
