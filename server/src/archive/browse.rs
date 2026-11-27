//! Looking into a ZIP file without extracting it: its list of entries, and the content of one of them. The archive is
//! read a part at a time, wherever it is stored: the list takes its end and its central directory, an entry only its
//! own part. The limits of extraction (archive.rs) hold here too.

use std::{
    io::{self, Read, Seek, SeekFrom},
    pin::Pin,
};

use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, header},
    response::Response,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{MAX_DIRECTORY_BYTES, MAX_EXTRACT_BYTES, MAX_EXTRACT_DEPTH, MAX_EXTRACT_ENTRIES, MAX_RATIO, RATIO_EXEMPT_BYTES, is_zip, too_compressed};
use crate::{
    auth::User,
    error::{AppError, AppResult},
    files::Source,
    state::AppState,
    tree::{self, Node},
    util::content_disposition,
    zip::ReadEntry,
};

/// What is read from the archive's storage at a time, at least and at most
const CHUNK: u64 = 64 * 1024;
const MAX_CHUNK: u64 = 8 * 1024 * 1024;
/// How much of an entry's compressed content is decompressed at a time: what comes out of it is held in memory
const PIECE: usize = 16 * 1024;

/// Lists of entries read at once: each may hold a directory of MAX_DIRECTORY_BYTES in memory, and unlike
/// extractions, which are jobs with a limit of their own, any number of requests may ask for one
static READING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

/// An archive read where it is stored, a part at a time, by blocking code (on a blocking thread)
struct Parts {
    st: AppState,
    source: Source,
    size: u64,
    pos: u64,
    /// The part last read, and where it starts
    part: Vec<u8>,
    start: u64,
    runtime: tokio::runtime::Handle,
}

impl Read for Parts {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.size || out.is_empty() {
            return Ok(0);
        }
        if self.pos < self.start || self.pos >= self.start + self.part.len() as u64 {
            let want = (out.len() as u64).clamp(CHUNK, MAX_CHUNK).min(self.size - self.pos);
            let (st, source, pos) = (&self.st, &self.source, self.pos);
            let part = self.runtime.block_on(async {
                let mut part = Vec::with_capacity(want as usize);
                source.open(st, pos, want).await?.take(want).read_to_end(&mut part).await?;
                io::Result::Ok(part)
            })?;
            if part.is_empty() {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            (self.part, self.start) = (part, pos);
        }
        let from = (self.pos - self.start) as usize;
        let n = out.len().min(self.part.len() - from);
        out[..n].copy_from_slice(&self.part[from..from + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Parts {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let to = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(n) => self.size.checked_add_signed(n),
            SeekFrom::Current(n) => self.pos.checked_add_signed(n),
        };
        self.pos = to.ok_or(io::ErrorKind::InvalidInput)?;
        Ok(self.pos)
    }
}

/// A ZIP file the user may download: its content, size and a tag that changes with it
async fn archive(st: &AppState, user: &User, id: &str) -> AppResult<(Node, Source, u64, String)> {
    let node = tree::readable_node(&mut *st.db.acquire().await?, user, id).await?;
    if !is_zip(&node) {
        return Err(AppError::bad_request("Only ZIP files can be opened this way"));
    }
    let source = Source::resolve(st, &node).await?;
    let (size, tag) = source.describe(node.size as u64).await?;
    Ok((node, source, size, tag))
}

/// Refuses a request made about another content than the archive has now
pub(super) fn same_version(asked: &str, now: &str) -> AppResult<()> {
    if asked == now { Ok(()) } else { Err(AppError::conflict("The ZIP file has changed. Open it again.")) }
}

fn damaged() -> AppError {
    AppError::bad_request("This ZIP file is damaged and can't be opened")
}

/// A failure to read the archive: damage, or its storage
fn unreadable(e: io::Error) -> AppError {
    match e.kind() {
        io::ErrorKind::FileTooLarge => AppError::bad_request(format!("This ZIP file holds more than {MAX_EXTRACT_ENTRIES} items, more than can be shown")),
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidInput => damaged(),
        _ => e.into(),
    }
}

/// Reads the archive's list of entries, and what `then` wants of the archive besides, on a blocking thread
async fn with_entries<T: Send + 'static>(
    st: &AppState,
    source: Source,
    size: u64,
    then: impl FnOnce(&mut Parts, Vec<ReadEntry>) -> AppResult<T> + Send + 'static,
) -> AppResult<T> {
    let mut parts = Parts { st: st.clone(), source, size, pos: 0, part: Vec::new(), start: 0, runtime: tokio::runtime::Handle::current() };
    let turn = READING.acquire().await.map_err(AppError::internal)?;
    tokio::task::spawn_blocking(move || {
        // Held until the reading ends, also when whoever asked has given up
        let _turn = turn;
        let entries = crate::zip::read_entries(&mut parts, MAX_EXTRACT_ENTRIES, MAX_DIRECTORY_BYTES).map_err(unreadable)?;
        then(&mut parts, entries)
    })
    .await?
}

/// The names on the way to an entry and its own, as the list shows them and extraction takes them; None for a path
/// that points outside the folder it would be extracted to, one deeper than can be extracted, and an empty one
pub(super) fn entry_path(name: &str) -> Option<Vec<String>> {
    let path = name.replace('\\', "/");
    if path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') {
        return None;
    }
    let parts: Vec<String> = path.split('/').filter(|p| !p.is_empty() && *p != ".").map(str::to_string).collect();
    let fine = !parts.is_empty() && parts.len() <= MAX_EXTRACT_DEPTH + 1 && parts.iter().all(|p| p != "..");
    fine.then_some(parts)
}

/// Why an entry's content can't be had, if it can't: "password", "method" (compressed in a way that isn't supported),
/// or "size" (larger, or growing far more, than extraction allows)
fn locked(e: &ReadEntry) -> Option<&'static str> {
    if e.encrypted {
        Some("password")
    } else if !e.supported() {
        Some("method")
    } else if e.size > MAX_EXTRACT_BYTES || (e.size > RATIO_EXEMPT_BYTES && e.size / MAX_RATIO > e.csize) {
        Some("size")
    } else {
        None
    }
}

#[derive(Serialize)]
pub struct Listed {
    /// The entry's number in the archive, to download it by
    pub index: usize,
    /// Its path inside the archive ("/" between folders)
    pub path: String,
    pub dir: bool,
    /// Size once extracted
    pub size: u64,
    /// The time the archive gives it, which says no time zone: seconds since 1970 of that clock time
    pub modified: Option<i64>,
    pub locked: Option<&'static str>,
}

#[derive(Serialize)]
pub struct Listing {
    /// The content listed: downloading and extracting entries name it
    pub version: String,
    pub entries: Vec<Listed>,
    /// Entries left out, whose paths point outside the folder they would be extracted to (or go too deep)
    pub hidden: usize,
}

/// What a ZIP file holds
pub async fn entries(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<Listing>> {
    let (_, source, size, version) = archive(&st, &user, &id).await?;
    let (entries, hidden) = with_entries(&st, source, size, |_, entries| {
        let mut hidden = 0;
        let listed = entries
            .iter()
            .enumerate()
            .filter_map(|(index, e)| {
                let Some(path) = entry_path(&e.name) else {
                    // A bare "./" names nothing
                    hidden += usize::from(!e.name.trim_matches(['.', '/']).is_empty() || e.name.contains(".."));
                    return None;
                };
                let (size, locked) = if e.is_dir { (0, None) } else { (e.size, locked(e)) };
                Some(Listed { index, path: path.join("/"), dir: e.is_dir, size, modified: e.modified, locked })
            })
            .collect();
        Ok((listed, hidden))
    })
    .await?;
    Ok(Json(Listing { version, entries, hidden }))
}

#[derive(Deserialize)]
pub struct EntryQuery {
    /// The `version` of the list the entry's number is from
    v: String,
}

/// Downloads one file out of a ZIP file
pub async fn entry(
    State(st): State<AppState>,
    user: User,
    Path((id, index)): Path<(String, usize)>,
    Query(q): Query<EntryQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    crate::files::refuse_as_code(&headers)?;
    let (_, source, size, version) = archive(&st, &user, &id).await?;
    same_version(&q.v, &version)?;
    let (entry, name, start) = with_entries(&st, source.clone(), size, move |parts, mut entries| {
        let not_there = || AppError::not_found("File not found");
        if index >= entries.len() {
            return Err(not_there());
        }
        let entry = entries.swap_remove(index);
        let name = entry_path(&entry.name).and_then(|mut p| p.pop()).filter(|_| !entry.is_dir).ok_or_else(not_there)?;
        match locked(&entry) {
            Some("password") => return Err(AppError::bad_request("This file is protected with a password, which isn't supported")),
            Some("method") => return Err(AppError::bad_request("This file is compressed in a way that isn't supported")),
            Some(_) => return Err(too_compressed()),
            None => {}
        }
        let start = crate::zip::data_start(parts, &entry).map_err(unreadable)?;
        Ok((entry, name, start))
    })
    .await?;
    let content = source.open(&st, start, entry.csize).await?;

    let len = entry.size;
    let mut res = Response::new(Body::from_stream(extracted(content, entry)));
    let h = res.headers_mut();
    // Saved, never shown: as other files (files.rs), nothing in it runs on this site
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    h.insert(header::CONTENT_LENGTH, len.into());
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox; default-src 'none'"));
    if let Ok(v) = HeaderValue::from_str(&content_disposition("attachment", &name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

/// The content of an entry as it comes out of its part of the archive. Never more than the archive states comes out,
/// and the last piece only once the size and the CRC are right: a damaged entry ends as a download that fails
fn extracted(content: Pin<Box<dyn AsyncRead + Send>>, entry: ReadEntry) -> impl futures_util::Stream<Item = io::Result<Bytes>> {
    let (send, receive) = tokio::sync::mpsc::channel::<io::Result<Bytes>>(2);
    tokio::spawn(async move {
        if let Err(e) = pump(content, &entry, &send).await {
            tracing::warn!("Couldn't read a file out of a ZIP file: {e}");
            let _ = send.send(Err(e)).await;
        }
    });
    futures_util::stream::unfold(receive, |mut receive| async move { receive.recv().await.map(|piece| (piece, receive)) })
}

async fn pump(mut content: Pin<Box<dyn AsyncRead + Send>>, entry: &ReadEntry, send: &tokio::sync::mpsc::Sender<io::Result<Bytes>>) -> io::Result<()> {
    let damaged = || io::Error::new(io::ErrorKind::InvalidData, "damaged ZIP entry");
    let mut inflate = entry.deflated().then(|| flate2::write::DeflateDecoder::new(Vec::new()));
    let (mut input, mut crc, mut size) = (vec![0u8; PIECE], flate2::Crc::new(), 0u64);
    // Sent once more has come, or once all of it checked
    let mut held = Vec::new();
    loop {
        let n = content.read(&mut input).await?;
        let out = match inflate.as_mut() {
            None => input[..n].to_vec(),
            Some(z) => {
                if n > 0 {
                    io::Write::write_all(z, &input[..n]).map_err(|_| damaged())?;
                } else {
                    z.try_finish().map_err(|_| damaged())?;
                }
                std::mem::take(z.get_mut())
            }
        };
        if !out.is_empty() {
            size += out.len() as u64;
            if size > entry.size {
                return Err(damaged());
            }
            crc.update(&out);
            let ready = std::mem::replace(&mut held, out);
            // Nobody is waiting any more: the download was given up
            if !ready.is_empty() && send.send(Ok(ready.into())).await.is_err() {
                return Ok(());
            }
        }
        if n == 0 {
            break;
        }
    }
    if size != entry.size || crc.sum() != entry.crc {
        return Err(damaged());
    }
    if !held.is_empty() {
        let _ = send.send(Ok(held.into())).await;
    }
    Ok(())
}

/// Of `entries`, those at or below the paths chosen (as the list shows them), their names without the folder all the
/// chosen ones are in, which the new folder stands for
pub(super) fn chosen(entries: Vec<ReadEntry>, paths: &[String]) -> AppResult<Vec<ReadEntry>> {
    let paths: Vec<Vec<String>> = paths.iter().filter_map(|p| entry_path(p)).collect();
    let mut common: Option<&[String]> = None;
    for path in &paths {
        let parent = &path[..path.len() - 1];
        common = Some(match common {
            None => parent,
            Some(c) => &c[..c.iter().zip(parent).take_while(|(a, b)| a == b).count()],
        });
    }
    let skip = common.map_or(0, <[String]>::len);
    // Looked up by each of an entry's folders, however many are chosen
    let wanted: std::collections::HashSet<&[String]> = paths.iter().map(Vec::as_slice).collect();
    let found: Vec<ReadEntry> = entries
        .into_iter()
        .filter_map(|mut e| {
            let path = entry_path(&e.name)?;
            if !(1..=path.len()).any(|n| wanted.contains(&path[..n])) {
                return None;
            }
            e.name = path[skip..].join("/");
            if e.is_dir {
                e.name.push('/');
            }
            Some(e)
        })
        .collect();
    if found.is_empty() {
        return Err(AppError::bad_request("The selected items aren't in this ZIP file"));
    }
    Ok(found)
}

#[cfg(test)]
mod tests;
