//! The sign-in methods linked to my account

use super::*;

#[derive(Serialize, sqlx::FromRow)]
pub struct IdentityRow {
    pub(super) provider: String,
    pub(super) email: String,
    pub(super) name: String,
    pub(super) created_at: i64,
    pub(super) last_login_at: Option<i64>,
}

pub async fn my_identities(State(st): State<AppState>, user: User) -> AppResult<Json<Value>> {
    let rows: Vec<IdentityRow> = sqlx::query_as("SELECT provider, email, name, created_at, last_login_at FROM user_identities WHERE user_id = ? ORDER BY provider")
        .bind(user.id)
        .fetch_all(&st.db)
        .await?;
    let cfg = st.sso.read().unwrap().clone();
    let available: Vec<&str> = PROVIDERS.iter().copied().filter(|p| cfg.provider(p).is_some_and(ProviderConfig::ready)).collect();
    Ok(Json(json!({ "linked": rows, "available": available })))
}

pub async fn unlink(
    State(st): State<AppState>,
    user: User,
    Path(provider): Path<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let removed = {
        let _w = st.write_lock.lock().await;
        // Without a password (an account created by single sign-on that never got one), the last linked account can't be removed
        let (password_hash, linked, this_one): (String, i64, i64) = sqlx::query_as(
            "SELECT password_hash,
                    (SELECT COUNT(*) FROM user_identities WHERE user_id = users.id),
                    (SELECT COUNT(*) FROM user_identities WHERE user_id = users.id AND provider = ?)
             FROM users WHERE id = ?",
        )
        .bind(&provider)
        .bind(user.id)
        .fetch_one(&st.db)
        .await?;
        if this_one == 0 {
            return Ok(Json(json!({ "ok": true })));
        }
        if password_hash == NO_PASSWORD && linked <= 1 {
            return Err(AppError::bad_request("This is the only way to sign in to this account. Ask an administrator to set a password first."));
        }
        sqlx::query("DELETE FROM user_identities WHERE user_id = ? AND provider = ?").bind(user.id).bind(&provider).execute(&st.db).await?.rows_affected()
    };
    if removed > 0 {
        record_login_via(&st, Some(user.id), &user.username, "sso_unlink", &provider, &client_ip(&st, addr, &headers), &headers);
    }
    Ok(Json(json!({ "ok": true })))
}
