//! A byte-ordered, bounded depth-first walk. Only a small child page per open directory is kept, never a whole tree.

use super::credentials::{Identity, request_identity};
use crate::{
    state::AppState,
    tree::{self, Need, Node},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use s3s::{S3Request, S3Response, S3Result, dto::*, s3_error};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Serialize, Deserialize)]
struct Cursor {
    bucket: String,
    prefix: String,
    delimiter: String,
    key: String,
    group: bool,
}

fn token(st: &AppState, cursor: &Cursor) -> S3Result<String> {
    let bytes = serde_json::to_vec(cursor).map_err(super::internal)?;
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&st.secret).map_err(super::internal)?;
    mac.update(&bytes);
    Ok(format!("{}.{}", URL_SAFE_NO_PAD.encode(bytes), URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())))
}

fn decode(st: &AppState, value: &str, bucket: &str, prefix: &str, delimiter: &str) -> S3Result<Cursor> {
    if value.len() > 6000 {
        return Err(s3_error!(InvalidArgument, "Invalid continuation token"));
    }
    let (data, signature) = value.split_once('.').ok_or_else(|| s3_error!(InvalidArgument))?;
    let bytes = URL_SAFE_NO_PAD.decode(data).map_err(|_| s3_error!(InvalidArgument))?;
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| s3_error!(InvalidArgument))?;
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&st.secret).map_err(super::internal)?;
    mac.update(&bytes);
    mac.verify_slice(&signature).map_err(|_| s3_error!(InvalidArgument))?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| s3_error!(InvalidArgument))?;
    if cursor.bucket != bucket || cursor.prefix != prefix || cursor.delimiter != delimiter {
        return Err(s3_error!(InvalidArgument));
    }
    Ok(cursor)
}

struct Frame {
    folder: Node,
    prefix: String,
    after: String,
    rows: VecDeque<Node>,
    scanned: bool,
}

struct Walk<'a> {
    st: &'a AppState,
    identity: &'a Identity,
    frames: Vec<Frame>,
    prefix: &'a str,
    after: &'a str,
    skip_group: bool,
}

impl<'a> Walk<'a> {
    fn new(st: &'a AppState, identity: &'a Identity, prefix: &'a str, after: &'a str, skip_group: bool) -> Self {
        Self {
            st,
            identity,
            frames: vec![Frame { folder: identity.root.clone(), prefix: String::new(), after: String::new(), rows: VecDeque::new(), scanned: false }],
            prefix,
            after,
            skip_group,
        }
    }

    async fn next(&mut self) -> S3Result<Option<(String, Node)>> {
        while let Some(frame) = self.frames.last_mut() {
            // A stopped key or publisher must fail the page; never turn it into a successful partial listing.
            super::credentials::identity(self.st, &self.identity.key.access_key, false).await?;
            if frame.rows.is_empty() {
                if !frame.scanned {
                    if frame.folder.in_folder_space() {
                        crate::folders::sync_folder(self.st, &frame.folder).await;
                    }
                    frame.scanned = true;
                }
                let lower = [self.prefix, self.after]
                    .into_iter()
                    .filter_map(|bound| bound.strip_prefix(&frame.prefix))
                    .map(|bound| match bound.split_once('/') {
                        Some((head, _)) => format!("{head}/"),
                        None => bound.into(),
                    })
                    .max()
                    .unwrap_or_default();
                let sql = format!(
                    "SELECT {} FROM nodes n WHERE n.parent_id = ?1 AND n.trashed_at IS NULL AND (n.name || CASE WHEN n.kind = 'folder' THEN '/' ELSE '' END) COLLATE BINARY > ?2 AND (n.name || CASE WHEN n.kind = 'folder' THEN '/' ELSE '' END) COLLATE BINARY >= ?3 ORDER BY (n.name || CASE WHEN n.kind = 'folder' THEN '/' ELSE '' END) COLLATE BINARY LIMIT 64",
                    tree::NODE_COLS
                );
                let rows: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
                    .bind(&frame.folder.id)
                    .bind(&frame.after)
                    .bind(lower)
                    .fetch_all(&self.st.db)
                    .await
                    .map_err(super::internal)?;
                if rows.is_empty() {
                    self.frames.pop();
                    continue;
                }
                frame.rows = rows.into();
            }
            let Some(node) = frame.rows.pop_front() else { continue };
            let name = format!("{}{}", node.name, if node.is_folder() { "/" } else { "" });
            frame.after = name.clone();
            let key = format!("{}{name}", frame.prefix);
            if key.len() > 1024 {
                continue;
            }
            // Skip branches before the cursor or outside the requested prefix without opening their children.
            let may_contain = node.is_folder()
                && (self.prefix.starts_with(&key) || key.starts_with(self.prefix))
                && !(key.as_bytes() < self.after.as_bytes() && !self.after.starts_with(&key))
                && !(self.skip_group && key.starts_with(self.after));
            let mut conn = self.st.db.acquire().await.map_err(super::internal)?;
            let live = tree::node_for(&mut conn, &self.identity.user, &node.id, Need::Read).await;
            drop(conn);
            match live {
                Ok(_) => (),
                Err(error) if error.status == axum::http::StatusCode::NOT_FOUND => continue,
                Err(error) => return Err(super::map_error(error)),
            }
            if may_contain {
                self.frames.push(Frame { folder: node.clone(), prefix: key.clone(), after: String::new(), rows: VecDeque::new(), scanned: false });
            }
            if key.starts_with(self.prefix) && key.as_bytes() > self.after.as_bytes() && !(self.skip_group && key.starts_with(self.after)) {
                return Ok(Some((key, node)));
            }
        }
        Ok(None)
    }

    fn skip(&mut self, group: &str) {
        while self.frames.last().is_some_and(|f| f.prefix.starts_with(group)) {
            self.frames.pop();
        }
    }
}

pub struct Page {
    pub objects: Vec<Object>,
    pub groups: Vec<CommonPrefix>,
    pub last: Option<(String, bool)>,
    pub truncated: bool,
}

pub fn encode(key: &str, enabled: bool) -> String {
    if enabled { percent_encoding::utf8_percent_encode(key, percent_encoding::NON_ALPHANUMERIC).to_string() } else { key.into() }
}

struct Listing<'a> {
    prefix: &'a str,
    delimiter: &'a str,
    after: &'a str,
    skip_group: bool,
    limit: i32,
    url: bool,
}

async fn page(st: &AppState, identity: &Identity, query: Listing<'_>) -> S3Result<Page> {
    let Listing { prefix, delimiter, after, skip_group, limit, url } = query;
    if prefix.len() > 1024 || delimiter.len() > 64 || after.len() > 1024 || !(0..=1000).contains(&limit) {
        return Err(s3_error!(InvalidArgument));
    }
    let mut page = Page { objects: vec![], groups: vec![], last: None, truncated: false };
    if limit == 0 {
        return Ok(page);
    }
    let mut walk = Walk::new(st, identity, prefix, after, skip_group);
    let mut previous_group = String::new();
    while let Some((key, node)) = walk.next().await? {
        let group = if delimiter.is_empty() { None } else { key[prefix.len()..].find(delimiter).map(|at| key[..prefix.len() + at + delimiter.len()].to_string()) };
        if let Some(group) = &group {
            walk.skip(group);
            if *group == previous_group || group.as_bytes() <= after.as_bytes() {
                continue;
            }
            previous_group = group.clone();
        }
        if page.objects.len() + page.groups.len() == limit as usize {
            page.truncated = true;
            break;
        }
        if let Some(group) = group {
            page.last = Some((group.clone(), true));
            page.groups.push(CommonPrefix { prefix: Some(encode(&group, url)) });
        } else {
            let description = super::etag::describe_node(st, &node).await?;
            page.last = Some((key.clone(), false));
            page.objects.push(Object {
                key: Some(encode(&key, url)),
                size: Some(description.size as i64),
                e_tag: Some(description.etag),
                last_modified: Some(super::objects::timestamp(node.updated_at)),
                ..Default::default()
            });
        }
    }
    Ok(page)
}

pub async fn v2(st: &AppState, req: S3Request<ListObjectsV2Input>) -> S3Result<S3Response<ListObjectsV2Output>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), false).await?;
    let i = req.input;
    let prefix = i.prefix.as_deref().unwrap_or_default();
    let delimiter = i.delimiter.as_deref().unwrap_or_default();
    let cursor = i.continuation_token.as_ref().map(|t| decode(st, t, &identity.bucket.id, prefix, delimiter)).transpose()?;
    let after = cursor.as_ref().map(|c| c.key.as_str()).or(i.start_after.as_deref()).unwrap_or_default();
    let limit = i.max_keys.unwrap_or(1000);
    let url = i.encoding_type.is_some();
    let page = page(st, &identity, Listing { prefix, delimiter, after, skip_group: cursor.as_ref().is_some_and(|c| c.group), limit, url }).await?;
    let next = if page.truncated {
        let (key, group) = page.last.clone().ok_or_else(|| s3_error!(InternalError))?;
        Some(token(st, &Cursor { bucket: identity.bucket.id, prefix: prefix.into(), delimiter: delimiter.into(), key, group })?)
    } else {
        None
    };
    Ok(S3Response::new(ListObjectsV2Output {
        name: Some(i.bucket),
        prefix: i.prefix.map(|v| encode(&v, url)),
        delimiter: i.delimiter.map(|v| encode(&v, url)),
        max_keys: Some(limit),
        key_count: Some((page.objects.len() + page.groups.len()) as i32),
        contents: Some(page.objects),
        common_prefixes: Some(page.groups),
        is_truncated: Some(page.truncated),
        continuation_token: i.continuation_token,
        next_continuation_token: next,
        start_after: i.start_after.map(|v| encode(&v, url)),
        encoding_type: i.encoding_type,
        ..Default::default()
    }))
}

pub async fn v1(st: &AppState, req: S3Request<ListObjectsInput>) -> S3Result<S3Response<ListObjectsOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), false).await?;
    let i = req.input;
    let prefix = i.prefix.as_deref().unwrap_or_default();
    let delimiter = i.delimiter.as_deref().unwrap_or_default();
    let after = i.marker.as_deref().unwrap_or_default();
    let limit = i.max_keys.unwrap_or(1000);
    let url = i.encoding_type.is_some();
    let page = page(st, &identity, Listing { prefix, delimiter, after, skip_group: !delimiter.is_empty() && after.ends_with(delimiter), limit, url }).await?;
    Ok(S3Response::new(ListObjectsOutput {
        name: Some(i.bucket),
        prefix: i.prefix.map(|v| encode(&v, url)),
        delimiter: i.delimiter.map(|v| encode(&v, url)),
        marker: i.marker.map(|v| encode(&v, url)),
        max_keys: Some(limit),
        contents: Some(page.objects),
        common_prefixes: Some(page.groups),
        is_truncated: Some(page.truncated),
        next_marker: page.last.filter(|_| page.truncated).map(|(key, _)| encode(&key, url)),
        encoding_type: i.encoding_type,
        ..Default::default()
    }))
}
