//! Changes to items: through the web's handlers, so they get the same checks and the same activity log

use axum::{
    Json,
    extract::{self, State},
};
use serde_json::{Value, json};

use crate::{
    auth::User,
    error::{AppError, AppResult},
    nodes,
    state::AppState,
    tree::{self, Node},
    util::numbered_name,
};

/// Calls a web handler with a JSON request
fn json_req<T: serde::de::DeserializeOwned>(v: Value) -> AppResult<Json<T>> {
    serde_json::from_value(v).map(Json).map_err(AppError::internal)
}

/// Makes a folder `name` in `parent`
pub async fn make_folder(st: &AppState, user: &User, parent: &Node, name: &str) -> AppResult<()> {
    nodes::create_folder(State(st.clone()), user.clone(), json_req(json!({ "parent_id": parent.id, "name": name }))?).await.map(|_| ())
}

pub async fn rename(st: &AppState, user: &User, id: &str, name: &str) -> AppResult<()> {
    nodes::rename(State(st.clone()), user.clone(), extract::Path(id.to_string()), json_req(json!({ "name": name }))?).await.map(|_| ())
}

/// Moves an item into another folder, content included: the call waits for all of it
pub async fn move_into(st: &AppState, user: &User, id: &str, dest: &str) -> AppResult<()> {
    let Json(req) = json_req(json!({ "ids": [id], "dest_id": dest }))?;
    if let Some(across) = nodes::move_items(st, user, &req).await? {
        nodes::run_content(st, user, across, &Default::default()).await?;
    }
    Ok(())
}

pub async fn trash(st: &AppState, user: &User, id: &str) -> AppResult<()> {
    nodes::trash(State(st.clone()), user.clone(), json_req(json!({ "ids": [id] }))?).await.map(|_| ())
}

/// Takes an item back out of the trash, where it was
pub async fn restore(st: &AppState, user: &User, id: &str) -> AppResult<()> {
    nodes::restore(State(st.clone()), user.clone(), json_req(json!({ "ids": [id] }))?).await.map(|_| ())
}

/// Moves an item into `dest` as `name`: a rename, a move, or both (in the order the names allow). When the second step
/// fails, the first is undone, so the item is where and as it was.
pub async fn move_to(st: &AppState, user: &User, src: &Node, dest: &Node, name: &str) -> AppResult<()> {
    let from = src.parent_id.clone().unwrap_or_default();
    if from == dest.id {
        return rename(st, user, &src.id, name).await;
    }
    if name == src.name {
        return move_into(st, user, &src.id, &dest.id).await;
    }
    // Best effort: the step that failed is what the client hears about
    let was_in = from.as_str();
    let back = |moved: bool, renamed: bool| async move {
        if moved {
            let _ = move_into(st, user, &src.id, was_in).await;
        }
        if renamed {
            let _ = rename(st, user, &src.id, &src.name).await;
        }
    };
    let mut c = st.db.acquire().await?;
    if !tree::name_taken(&mut c, &dest.id, &src.name).await? {
        drop(c);
        move_into(st, user, &src.id, &dest.id).await?;
        if let Err(e) = rename(st, user, &src.id, name).await {
            back(true, false).await;
            return Err(e);
        }
        return Ok(());
    }
    if !tree::name_taken(&mut c, &from, name).await? {
        drop(c);
        rename(st, user, &src.id, name).await?;
        if let Err(e) = move_into(st, user, &src.id, &dest.id).await {
            back(false, true).await;
            return Err(e);
        }
        return Ok(());
    }
    // Both names are taken on the other side: pass through a name free in both folders
    let mut via = None;
    for n in 1..10_000 {
        let candidate = numbered_name(name, n, src.is_folder());
        if !tree::name_taken(&mut c, &dest.id, &candidate).await? && !tree::name_taken(&mut c, &from, &candidate).await? {
            via = Some(candidate);
            break;
        }
    }
    drop(c);
    let via = via.ok_or_else(|| AppError::conflict("Too many items with the same name"))?;
    rename(st, user, &src.id, &via).await?;
    if let Err(e) = move_into(st, user, &src.id, &dest.id).await {
        back(false, true).await;
        return Err(e);
    }
    if let Err(e) = rename(st, user, &src.id, name).await {
        back(true, true).await;
        return Err(e);
    }
    Ok(())
}

/// Copies an item into `dest` as `name`: the copy is made under that name, so nothing made there at the same time
/// (another copy) can be taken for it
pub async fn copy_to(st: &AppState, user: &User, src: &Node, dest: &Node, name: &str) -> AppResult<()> {
    let Json(req) = json_req(json!({ "ids": [src.id], "dest_id": dest.id, "name": name }))?;
    if let Some(across) = nodes::copy_items(st, user, &req).await? {
        nodes::run_content(st, user, across, &Default::default()).await?;
    }
    Ok(())
}
