//! The interface style a person chose: `auto`, `windows` or `mac` (`users.ui_style`). It is kept with the account so it
//! follows them to every device; the browser decides what `auto` means there, by its operating system
//! (web/src/lib/style.ts). The server only stores it: nothing it sends depends on it.

use axum::{Json, extract::State};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    auth::User,
    error::{AppError, AppResult},
    state::AppState,
};

/// The choices, as stored and as the API names them
pub const STYLES: [&str; 3] = ["auto", "windows", "mac"];

/// The style saved with an account
pub async fn saved(st: &AppState, user_id: i64) -> AppResult<String> {
    let (style,): (String,) = sqlx::query_as("SELECT ui_style FROM users WHERE id = ?").bind(user_id).fetch_one(&st.db).await?;
    Ok(style)
}

#[derive(Deserialize)]
pub struct StyleReq {
    style: String,
}

/// Saves the interface style the person chose with their account
pub async fn save(State(st): State<AppState>, user: User, Json(req): Json<StyleReq>) -> AppResult<Json<Value>> {
    let style = STYLES.into_iter().find(|s| *s == req.style).ok_or_else(|| AppError::bad_request("Unknown interface style"))?;
    let _w = st.write_lock.lock().await;
    sqlx::query("UPDATE users SET ui_style = ? WHERE id = ?").bind(style).bind(user.id).execute(&st.db).await?;
    Ok(Json(json!({ "style": style })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn the_chosen_style_is_saved_with_the_account() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        assert_eq!(saved(&env.st, amy.id).await.unwrap(), "auto");

        let set = |style: &str| save(State(env.st.clone()), amy.clone(), Json(StyleReq { style: style.into() }));
        for wrong in ["", "Mac", "linux", "auto "] {
            assert!(set(wrong).await.is_err(), "{wrong:?}");
        }
        assert_eq!(saved(&env.st, amy.id).await.unwrap(), "auto");
        for style in ["mac", "windows", "auto", "mac"] {
            let Json(answer) = set(style).await.unwrap();
            assert_eq!(answer["style"], style);
            assert_eq!(saved(&env.st, amy.id).await.unwrap(), style);
        }
        // The page learns it with the rest of the account
        let Json(me) = crate::signin::me(State(env.st.clone()), amy.clone(), axum::http::HeaderMap::new()).await.unwrap();
        assert_eq!(me.style, "mac");
        // Only their own
        assert_eq!(saved(&env.st, ben.id).await.unwrap(), "auto");
        // The database refuses anything else too
        assert!(sqlx::query("UPDATE users SET ui_style = 'linux' WHERE id = ?").bind(amy.id).execute(&env.st.db).await.is_err());
    }
}
