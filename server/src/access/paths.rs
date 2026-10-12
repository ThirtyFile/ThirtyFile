//! Finding items by path: what a path names, and where a new item at a path goes

use sqlx::SqliteConnection;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    paths::{Target, resolve},
    state::AppState,
    tree::Node,
    util::validate_name,
};

/// The folder a new item at this path goes into, and the item's name
pub async fn parent_and_name(conn: &mut SqliteConnection, user: &User, segs: &[String]) -> AppResult<(Node, String)> {
    let Some((last, parent)) = segs.split_last() else { return Err(AppError::forbidden("Spaces can't be changed here")) };
    let found = resolve(conn, user, parent).await?.ok_or_else(|| AppError::conflict("The folder doesn't exist"))?;
    let folder = match found.target {
        Target::Node(n) if n.is_folder() => *n,
        Target::Node(_) => return Err(AppError::conflict("The folder doesn't exist")),
        Target::Root | Target::Shared => return Err(AppError::forbidden("Items can only be added inside a space or a shared folder")),
    };
    let name = validate_name(last)?;
    if name != *last {
        return Err(AppError::bad_request("Name can't start or end with a space"));
    }
    // Refused as a client expects for its temporary and system files (.DS_Store, Thumbs.db…) where it can't write them
    if folder.in_folder_space() {
        crate::fsops::check_name(&name).map_err(|e| AppError::forbidden(e.message))?;
    }
    Ok((folder, name))
}

/// The item a path names, to change it: not the top of the tree or Shared with me, which aren't items
pub async fn item(st: &AppState, user: &User, segs: &[String]) -> AppResult<Node> {
    match resolve(&mut *st.db.acquire().await?, user, segs).await?.ok_or_else(|| AppError::not_found("Not found"))?.target {
        Target::Node(n) => Ok(*n),
        Target::Root | Target::Shared => Err(AppError::forbidden("This folder can't be changed")),
    }
}
