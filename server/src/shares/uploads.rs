//! Sending files through a link that accepts them

use super::*;

/// The cookie holding a browser's key for the uploads it starts through link `token`
fn visitor_cookie(token: &str) -> String {
    format!("tf_up_{token}")
}

/// A link that accepts files, opened (password, expiry, the creator's access), with the account the files will belong to
pub(super) async fn upload_share(st: &AppState, token: &str, headers: &HeaderMap, visitor: Visitor) -> AppResult<upload::Uploader> {
    let (share, root) = open_share(st, token, headers).await?;
    if !share.allow_upload || !root.is_folder() {
        return Err(AppError::forbidden("This link doesn't accept files"));
    }
    // The creator's current permissions decide (open_share checked they can still share it; uploading checks they
    // can still change the folder, and their space's quota)
    let user = auth::user_by_id(st, &mut *st.db.acquire().await?, share.owner_id).await?.ok_or_else(gone)?;
    // Each browser's uploads are its own: other visitors of the link can't continue, look at or cancel them. A browser
    // without a key gets one with the upload it starts (an upload id from elsewhere then matches nothing).
    let name = visitor_cookie(&share.id);
    let (key, new_key) = match auth::get_cookie(headers, &name).filter(|k| (32..=128).contains(&k.len())) {
        Some(k) => (k.to_string(), None),
        None => {
            let k = crate::util::random_token(43);
            let path = format!("/api/public/shares/{}", share.id);
            let cookie = auth::cookie_header(st, &name, &k, &path, upload::UPLOAD_TTL);
            (k, Some(cookie))
        }
    };
    let visitor_key = crate::util::sha256_hex(key.as_bytes());
    Ok(upload::Uploader { user, share: Some(upload::ShareUpload { id: share.id, root: root.id, drop_only: share.drop_only, visitor, visitor_key, new_key }) })
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

/// In tests: the headers `h` as sent by browser `who`, which holds its key for the uploads it starts through link `token`
#[cfg(test)]
pub(super) fn from_browser(mut h: HeaderMap, token: &str, who: &str) -> HeaderMap {
    let cookie = format!("{}={:0>40}", visitor_cookie(token), who);
    h.append(header::COOKIE, cookie.parse().unwrap());
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn visitor() -> Visitor {
        Visitor { ip: "203.0.113.5".into(), user_agent: String::new() }
    }

    async fn link(env: &testutil::TestEnv, owner: &User, folder: &str) -> String {
        let req = CreateReq {
            node_id: folder.to_string(),
            password: None,
            expires_at: None,
            max_downloads: None,
            allow_upload: true,
            drop_only: true,
            allow_download: false,
        };
        create(State(env.st.clone()), owner.clone(), Json(req)).await.unwrap().0.id
    }

    /// Starts an upload of `len` bytes through a link and sends the first of them; returns its id
    async fn half_sent(env: &testutil::TestEnv, token: &str, len: usize, first: &'static [u8]) -> String {
        use base64::Engine;
        let mut h = HeaderMap::new();
        h.insert("upload-length", len.to_string().parse().unwrap());
        h.insert("upload-metadata", format!("filename {}", base64::engine::general_purpose::STANDARD.encode("report.pdf")).parse().unwrap());
        let res = public_upload_create(State(env.st.clone()), Path(token.to_string()), from_browser(h, token, "first"), visitor()).await.unwrap();
        let id = res.headers()[header::LOCATION].to_str().unwrap().rsplit('/').next().unwrap().to_string();
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, "application/offset+octet-stream".parse().unwrap());
        h.insert("upload-offset", "0".parse().unwrap());
        let res = public_upload_patch(
            State(env.st.clone()),
            Path((token.to_string(), id.clone())),
            from_browser(h, token, "first"),
            visitor(),
            axum::body::Body::from(first),
        )
        .await
        .unwrap();
        assert_eq!(res.headers()["upload-offset"], first.len().to_string().as_str());
        id
    }

    async fn pending(env: &testutil::TestEnv, id: &str) -> bool {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM uploads WHERE id = ?").bind(id).fetch_one(&env.st.db).await.unwrap() == 1
    }

    #[tokio::test]
    async fn a_visitor_cancels_an_upload_through_its_link_and_only_there() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let inbox = env.folder(&amy, amy.root(), "Inbox").await;
        let other = env.folder(&amy, amy.root(), "Other").await;
        let (token, elsewhere) = (link(&env, &amy, &inbox).await, link(&env, &amy, &other).await);
        let id = half_sent(&env, &token, 10, b"hello").await;
        let part = env.dir.join("tmp").join(format!("upload-{id}"));
        assert!(part.is_file() && pending(&env, &id).await);

        // A browser without a key is given one with its first upload, kept for that link only
        let mut h = HeaderMap::new();
        h.insert("upload-length", "1".parse().unwrap());
        h.insert("upload-metadata", "filename YS50eHQ=".parse().unwrap());
        let res = public_upload_create(State(env.st.clone()), Path(token.clone()), h.clone(), visitor()).await.unwrap();
        let set = res.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(set.starts_with(&format!("tf_up_{token}=")) && set.contains(&format!("Path=/api/public/shares/{token};")) && set.contains("HttpOnly"), "{set}");
        let res = public_upload_create(State(env.st.clone()), Path(token.clone()), from_browser(h, &token, "first"), visitor()).await.unwrap();
        assert!(res.headers().get(header::SET_COOKIE).is_none(), "a browser with a key keeps it");

        // Another link, even of the same person, doesn't reach it
        let err = public_upload_delete(State(env.st.clone()), Path((elsewhere.clone(), id.clone())), from_browser(HeaderMap::new(), &elsewhere, "first"), visitor())
            .await
            .unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        // Nor does its owner's own upload address: it isn't one of theirs
        assert_eq!(upload::delete_as(&env.st, &upload::Uploader { user: amy.clone(), share: None }, &id).await.unwrap_err().status, StatusCode::NOT_FOUND);
        // Nor does another browser opening the same link, with or without a key of its own
        for other in [HeaderMap::new(), from_browser(HeaderMap::new(), &token, "second")] {
            let err = public_upload_head(State(env.st.clone()), Path((token.clone(), id.clone())), other.clone(), visitor()).await.unwrap_err();
            assert_eq!(err.status, StatusCode::NOT_FOUND);
            let err = public_upload_delete(State(env.st.clone()), Path((token.clone(), id.clone())), other, visitor()).await.unwrap_err();
            assert_eq!(err.status, StatusCode::NOT_FOUND);
        }
        assert!(pending(&env, &id).await);

        let res = public_upload_delete(State(env.st.clone()), Path((token.clone(), id.clone())), from_browser(HeaderMap::new(), &token, "first"), visitor())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_eq!(res.headers()["tus-resumable"], "1.0.0");
        assert!(!pending(&env, &id).await, "the upload is forgotten");
        assert!(!part.exists(), "what arrived of it is removed");
        let err = public_upload_head(State(env.st.clone()), Path((token.clone(), id.clone())), from_browser(HeaderMap::new(), &token, "first"), visitor())
            .await
            .unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        let (files,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE parent_id = ?").bind(&inbox).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(files, 0);

        // A link that no longer takes files can't be used to cancel either
        let id = half_sent(&env, &token, 10, b"again").await;
        sqlx::query("UPDATE shares SET allow_upload = 0 WHERE id = ?").bind(&token).execute(&env.st.db).await.unwrap();
        let err = public_upload_delete(State(env.st.clone()), Path((token.clone(), id.clone())), from_browser(HeaderMap::new(), &token, "first"), visitor())
            .await
            .unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert!(pending(&env, &id).await);
    }
}
