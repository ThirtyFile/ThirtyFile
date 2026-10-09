//! The WebDAV methods

use super::*;

pub(super) async fn propfind(st: &AppState, user: &User, segs: &[String], headers: &HeaderMap, body: Body) -> AppResult<Response> {
    let depth = match headers.get("depth").and_then(|v| v.to_str().ok()).map(str::trim) {
        Some("0") => 0,
        Some("1") | None => 1,
        Some(d) if d.eq_ignore_ascii_case("infinity") => {
            let xml = r#"<?xml version="1.0" encoding="utf-8"?><D:error xmlns:D="DAV:"><D:propfind-finite-depth/></D:error>"#;
            return Ok((StatusCode::FORBIDDEN, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], xml).into_response());
        }
        Some(_) => return Err(AppError::bad_request("Depth must be 0 or 1")),
    };
    let want = read_xml(body).await?.want();
    let with_quota = want.asks_for("quota-used-bytes") || want.asks_for("quota-available-bytes");
    let mut quotas = HashMap::new();
    let mut c = st.db.acquire().await?;
    let found = resolve(&mut c, user, segs).await?.ok_or_else(not_found)?;
    let mut out = Multistatus::new();
    let path = found.path;
    let child = |name: &str| {
        let mut p = path.clone();
        p.push(name.to_string());
        p
    };
    match found.target {
        Target::Root => {
            out.add(&href(&[], true), &want, folder_props("ThirtyFile"));
            if depth == 1 {
                for (name, root) in tops(&mut c, user).await?.spaces {
                    let Some(node) = tree::get_node(&mut c, &root).await? else { continue };
                    let q = if with_quota { quota(&mut c, &mut quotas, node.drive()).await? } else { None };
                    out.add(&href(&child(&name), true), &want, node_props(&node, &name, q));
                }
                out.add(&href(&child(SHARED), true), &want, folder_props(SHARED));
            }
        }
        Target::Shared => {
            out.add(&href(&path, true), &want, folder_props(SHARED));
            if depth == 1 {
                for (name, node) in tops(&mut c, user).await?.shared {
                    let q = if with_quota && node.is_folder() { quota(&mut c, &mut quotas, node.drive()).await? } else { None };
                    out.add(&href(&child(&name), node.is_folder()), &want, node_props(&node, &name, q));
                }
            }
        }
        Target::Node(node) => {
            let display = path.last().cloned().unwrap_or_default();
            let q = if with_quota && node.is_folder() { quota(&mut c, &mut quotas, node.drive()).await? } else { None };
            out.add(&href(&path, node.is_folder()), &want, node_props(&node, &display, q));
            if depth == 1 && node.is_folder() {
                if node.in_folder_space() {
                    // Changes made on the server's folder show up when it is opened, as on the web (without holding
                    // a connection meanwhile: syncing takes its own)
                    drop(c);
                    crate::folders::sync_opened(st, &node).await;
                    c = st.db.acquire().await?;
                }
                let mut list = nodes::list_children(&mut c, &node.id, &nodes::ListQuery::default()).await?;
                for n in std::mem::take(list.items_mut()) {
                    let q = if with_quota && n.is_folder() { quota(&mut c, &mut quotas, n.drive()).await? } else { None };
                    out.add(&href(&child(&n.name), n.is_folder()), &want, node_props(&n, &n.name, q));
                }
            }
        }
    }
    Ok(out.finish())
}

/// Accepted and not stored: every property is reported as set
pub(super) async fn proppatch(st: &AppState, user: &User, segs: &[String], body: Body) -> AppResult<Response> {
    let parsed = read_xml(body).await?;
    let found = resolve(&mut *st.db.acquire().await?, user, segs).await?.ok_or_else(not_found)?;
    let collection = !matches!(&found.target, Target::Node(n) if !n.is_folder());
    let mut out = Multistatus::new();
    let props: String = parsed.props.iter().map(|(ns, local)| foreign(ns, local)).collect();
    out.xml.push_str(&format!("<D:response><D:href>{}</D:href>", escape(href(&found.path, collection))));
    out.propstat(&props, "200 OK");
    out.xml.push_str("</D:response>");
    Ok(out.finish())
}

pub(super) async fn get(st: &AppState, user: &User, segs: &[String], headers: &HeaderMap) -> AppResult<Response> {
    match resolve(&mut *st.db.acquire().await?, user, segs).await?.ok_or_else(not_found)?.target {
        Target::Node(node) if !node.is_folder() => files::serve_blob(st, headers, files::node_blob(st, &node).await?, false).await,
        _ => Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "This is a WebDAV folder of ThirtyFile. Open it with a WebDAV client, or map it as a network drive.\n",
        )
            .into_response()),
    }
}

pub(super) async fn delete(st: &AppState, user: &User, segs: &[String]) -> AppResult<Response> {
    let node = found_node(st, user, segs).await?;
    if node.parent_id.is_none() {
        return Err(AppError::forbidden("A space can't be deleted here"));
    }
    trash(st, user, &node.id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub(super) async fn mkcol(st: &AppState, user: &User, segs: &[String], body: Body) -> AppResult<Response> {
    let bytes = axum::body::to_bytes(body, MAX_XML).await.map_err(|_| AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "The request is too large"))?;
    if !bytes.is_empty() {
        return Err(AppError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "MKCOL doesn't take a body"));
    }
    let (parent, name) = {
        let mut c = st.db.acquire().await?;
        let (parent, name) = parent_of(&mut c, user, segs).await?;
        if child_named(&mut c, &parent.id, &name).await?.is_some() {
            return Err(AppError::new(StatusCode::METHOD_NOT_ALLOWED, "An item with this name already exists"));
        }
        (parent, name)
    };
    let _ = nodes::create_folder(State(st.clone()), user.clone(), json_req(json!({ "parent_id": parent.id, "name": name }))?).await?;
    Ok(StatusCode::CREATED.into_response())
}

/// How long MOVE and COPY wait for the change before answering 202 Accepted. Clients expect them to be done when they
/// answer, and most do, well within this; a large copy to or from a folder on the server may not be: it goes on in the
/// background, and the client sees the result when it looks again. (Any 2xx is success to clients, and none of them
/// keeps waiting for minutes.) Tests wait as for any job (`jobs::wait`: long enough for a busy machine, unless they ask
/// for `jobs::short_wait`).
pub(super) fn transfer_wait() -> std::time::Duration {
    if cfg!(test) { jobs::wait() } else { std::time::Duration::from_secs(60) }
}

/// MOVE and COPY, with Destination and Overwrite (an item already at the destination goes to the trash)
pub(super) async fn transfer(st: &AppState, user: &User, segs: &[String], headers: &HeaderMap, moving: bool) -> AppResult<Response> {
    let dest_segs = destination(headers)?;
    let overwrite = !headers.get("overwrite").is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"F"));
    let src = found_node(st, user, segs).await?;
    if src.parent_id.is_none() {
        return Err(AppError::forbidden("A space can't be moved or copied here"));
    }
    let mut c = st.db.acquire().await?;
    let (dest, name) = parent_of(&mut c, user, &dest_segs).await?;
    if tree::is_within(&mut c, &dest.id, &src.id).await? {
        return Err(AppError::forbidden("A folder can't go into itself"));
    }
    let mut replaced = None;
    match child_named(&mut c, &dest.id, &name).await? {
        Some(existing) if existing.id == src.id => {
            if !moving {
                return Err(AppError::forbidden("The source and destination are the same"));
            }
        }
        Some(existing) => {
            if !overwrite {
                return Err(precondition("The destination already exists"));
            }
            replaced = Some(existing.id);
        }
        None => {}
    }
    drop(c);
    // The change runs as a job, so a client that gives up waiting (or loses its connection) doesn't cut it off halfway
    let done = if replaced.is_some() { StatusCode::NO_CONTENT } else { StatusCode::CREATED };
    let (st, user) = (st.clone(), user.clone());
    let job = jobs::run(&st.clone(), &user.clone(), "webdav", Limit::None, transfer_wait(), move |_| async move {
        if let Some(existing) = replaced {
            trash(&st, &user, &existing).await?;
        }
        if moving {
            move_to(&st, &user, &src, &dest, &name).await?;
        } else {
            copy_to(&st, &user, &src, &dest, &name).await?;
        }
        Ok(Outcome::default())
    })
    .await?;
    // Still going: accepted, and done in the background
    Ok(if job.running() { StatusCode::ACCEPTED } else { done }.into_response())
}

/// Moves an item into `dest` as `name`: a rename, a move, or both (in the order the names allow)
pub(super) async fn move_to(st: &AppState, user: &User, src: &Node, dest: &Node, name: &str) -> AppResult<()> {
    let from = src.parent_id.clone().unwrap_or_default();
    if from == dest.id {
        return rename(st, user, &src.id, name).await;
    }
    if name == src.name {
        return move_into(st, user, &src.id, &dest.id).await;
    }
    let mut c = st.db.acquire().await?;
    if !tree::name_taken(&mut c, &dest.id, &src.name).await? {
        drop(c);
        move_into(st, user, &src.id, &dest.id).await?;
        return rename(st, user, &src.id, name).await;
    }
    if !tree::name_taken(&mut c, &from, name).await? {
        drop(c);
        rename(st, user, &src.id, name).await?;
        return move_into(st, user, &src.id, &dest.id).await;
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
    move_into(st, user, &src.id, &dest.id).await?;
    rename(st, user, &src.id, name).await
}

/// (id, name) of a folder's items of one kind, newest first
pub(super) async fn children_of(conn: &mut SqliteConnection, parent_id: &str, kind: &str) -> AppResult<Vec<(String, String)>> {
    Ok(sqlx::query_as("SELECT id, name FROM nodes WHERE parent_id = ? AND kind = ? AND trashed_at IS NULL ORDER BY created_at DESC")
        .bind(parent_id)
        .bind(kind)
        .fetch_all(conn)
        .await?)
}

/// Copies an item into `dest` as `name` (the copy gets a free name first, then the one asked for)
pub(super) async fn copy_to(st: &AppState, user: &User, src: &Node, dest: &Node, name: &str) -> AppResult<()> {
    let before: HashSet<String> = children_of(&mut *st.db.acquire().await?, &dest.id, &src.kind).await?.into_iter().map(|(id, _)| id).collect();
    let Json(req) = json_req(json!({ "ids": [src.id], "dest_id": dest.id }))?;
    if let Some(across) = nodes::copy_items(st, user, &req).await? {
        nodes::run_content(st, user, across, &Default::default()).await?;
    }
    let after = children_of(&mut *st.db.acquire().await?, &dest.id, &src.kind).await?;
    let (id, copied) = after.into_iter().find(|(id, _)| !before.contains(id)).ok_or_else(|| AppError::internal("the copy wasn't found"))?;
    if copied != name {
        rename(st, user, &id, name).await?;
    }
    Ok(())
}

/// Hands out a lock (see the module notes: not enforced). A path that doesn't exist yet becomes an empty file, as
/// RFC 4918 asks; a refresh (no body, the token in `If`) gets the same token back
pub(super) async fn lock(st: &AppState, user: &User, segs: &[String], headers: &HeaderMap, body: Body) -> AppResult<Response> {
    let parsed = read_xml(body).await?;
    let refresh = headers.get("if").and_then(|v| v.to_str().ok()).and_then(|v| {
        let start = v.find("<opaquelocktoken:")? + 1;
        let end = start + v[start..].find('>')?;
        Some(v[start..end].to_string())
    });
    let found = resolve(&mut *st.db.acquire().await?, user, segs).await?;
    let (status, path, folder) = match found {
        Some(Found { target: Target::Node(node), path }) => {
            tree::node_for(&mut *st.db.acquire().await?, user, &node.id, Need::Write).await?;
            (StatusCode::OK, path, node.is_folder())
        }
        Some(_) => return Err(AppError::forbidden("This folder can't be locked")),
        None => {
            let (parent, name) = parent_of(&mut *st.db.acquire().await?, user, segs).await?;
            let parent = tree::folder_for(&mut *st.db.acquire().await?, user, &parent.id, Need::Write).await?;
            let tmp = st.tmp_dir().join(format!("dav-{}", new_id()));
            tokio::fs::write(&tmp, b"").await?;
            let mut path = segs[..segs.len() - 1].to_vec();
            path.push(name.clone());
            tokio::spawn(store(st.clone(), user.clone(), parent, name, tmp, 0, None)).await.map_err(AppError::internal)??;
            (StatusCode::CREATED, path, false)
        }
    };
    let token = refresh.unwrap_or_else(|| format!("opaquelocktoken:{}", uuid::Uuid::new_v4()));
    let timeout = headers
        .get("timeout")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').find_map(|t| t.trim().strip_prefix("Second-")?.parse::<u64>().ok()))
        .unwrap_or(3600)
        .min(86400);
    let depth = if folder && headers.get("depth").and_then(|v| v.to_str().ok()) != Some("0") { "infinity" } else { "0" };
    let owner = parsed.owner.trim();
    let owner = if owner.is_empty() { String::new() } else { format!("<D:owner><D:href>{}</D:href></D:owner>", escape(owner)) };
    let scope = if parsed.shared { "<D:shared/>" } else { "<D:exclusive/>" };
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><D:prop xmlns:D="DAV:"><D:lockdiscovery><D:activelock><D:locktype><D:write/></D:locktype><D:lockscope>{scope}</D:lockscope><D:depth>{depth}</D:depth>{owner}<D:timeout>Second-{timeout}</D:timeout><D:locktoken><D:href>{token}</D:href></D:locktoken><D:lockroot><D:href>{}</D:href></D:lockroot></D:activelock></D:lockdiscovery></D:prop>"#,
        escape(href(&path, folder)),
        token = escape(&token),
    );
    let mut res = (status, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], xml).into_response();
    if let Ok(v) = format!("<{token}>").parse::<axum::http::HeaderValue>() {
        res.headers_mut().insert("lock-token", v);
    }
    Ok(res)
}
