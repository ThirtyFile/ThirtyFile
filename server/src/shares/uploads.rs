//! Sending files through a link that accepts them

use super::*;

/// A link that accepts files, opened (password, expiry, the creator's access), with the account the files will belong to
pub(super) async fn upload_share(st: &AppState, token: &str, headers: &HeaderMap, visitor: Visitor) -> AppResult<upload::Uploader> {
    let (share, root) = open_share(st, token, headers).await?;
    if !share.allow_upload || !root.is_folder() {
        return Err(AppError::forbidden("This link doesn't accept files"));
    }
    // The creator's current permissions decide (open_share checked they can still share it; uploading checks they
    // can still change the folder, and their space's quota)
    let user = auth::user_by_id(st, &mut *st.db.acquire().await?, share.owner_id).await?.ok_or_else(gone)?;
    Ok(upload::Uploader { user, share: Some(upload::ShareUpload { id: share.id, root: root.id, drop_only: share.drop_only, visitor }) })
}

pub async fn public_upload_create(State(st): State<AppState>, Path(token): Path<String>, headers: HeaderMap, visitor: Visitor) -> AppResult<Response> {
    let up = upload_share(&st, &token, &headers, visitor).await?;
    upload::create_as(&st, &up, &headers).await
}

pub async fn public_upload_head(State(st): State<AppState>, Path((token, id)): Path<(String, String)>, headers: HeaderMap, visitor: Visitor) -> AppResult<Response> {
    let up = upload_share(&st, &token, &headers, visitor).await?;
    upload::head_as(&st, &up, &id).await
}

pub async fn public_upload_patch(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
    visitor: Visitor,
    body: axum::body::Body,
) -> AppResult<Response> {
    let up = upload_share(&st, &token, &headers, visitor).await?;
    upload::patch_as(&st, &up, &id, &headers, body).await
}

pub async fn public_upload_delete(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
    visitor: Visitor,
) -> AppResult<Response> {
    let up = upload_share(&st, &token, &headers, visitor).await?;
    upload::delete_as(&st, &up, &id).await
}
