//! Bounded reads shared by file protocols; each call rechecks the current account's access.

use tokio::io::AsyncReadExt;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    files::Source,
    state::AppState,
    tree::{self, Need, Node},
};

pub async fn read_at(st: &AppState, user: &User, id: &str, offset: u64, len: u32) -> AppResult<Vec<u8>> {
    let node = tree::node_for(&mut *st.db.acquire().await?, user, id, Need::Read).await?;
    if node.is_folder() {
        return Err(AppError::bad_request("This is a folder"));
    }
    let source = Source::resolve(st, &node).await?;
    let (size, _) = source.describe(node.size as u64).await?;
    let len = u64::from(len.min(256 * 1024)).min(size.saturating_sub(offset));
    if len == 0 {
        return Ok(Vec::new());
    }
    let mut reader = source.open(st, offset, len).await?;
    let mut out = Vec::with_capacity(len as usize);
    reader.read_to_end(&mut out).await?;
    Ok(out)
}

pub async fn list_page(st: &AppState, user: &User, folder: &str, after: Option<String>) -> AppResult<(Vec<Node>, Option<String>)> {
    let node = tree::folder_for(&mut *st.db.acquire().await?, user, folder, Need::Read).await?;
    if after.is_none() && node.in_folder_space() {
        crate::folders::sync_opened(st, &node).await;
    }
    let mut c = st.db.acquire().await?;
    let mut list = crate::nodes::list_children(&mut c, folder, &crate::nodes::ListQuery::page(after, 128).visible_to(user)).await?;
    let next = list.next().map(str::to_string);
    Ok((std::mem::take(list.items_mut()), next))
}
