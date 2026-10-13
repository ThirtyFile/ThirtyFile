//! Moving and copying between spaces, where one of them (or both) is a folder space

use super::*;

/// Content of items being moved or copied to another space, put in place before the index changes
pub(super) enum Placed {
    /// In the destination folder on disk, as `tmp` inside a folder `wrap` that scans ignore (`.thirtyfile-move-…`, so
    /// that after a restart in between, the item is still found there under its name); `renamed_from`: the original
    /// was renamed there (same disk), so undoing renames it back; `copied`: the originals of a move that copied them
    /// (another disk), removed once the index follows; `placed`: where `put_in_place` renamed it to, so that should the
    /// index not follow after all, `undo` takes it back from there
    Disk { wrap: Pinned, tmp: Pinned, renamed_from: Option<Pinned>, copied: Option<CopiedTree>, placed: Box<crate::sync::Mutex<Option<Pinned>>> },
    /// In the destination's content store: one staged content per file, and the file's size and modification time as
    /// it was stored (by node id)
    Store(HashMap<String, StagedBlob>, HashMap<String, (u64, i64)>),
}

/// The scan locks of the folder spaces a move or copy across spaces touches, taken in the order `lock` takes them: held
/// while an item leaves its folder and while the index follows it into the destination's, so a scan never finds it gone
/// from one folder, or there in the other, before the index says so. They are let go while content is copied or
/// written, which can take long, and taken again for the index to follow.
pub(super) struct Turn {
    st: AppState,
    drives: Vec<String>,
    held: Vec<OwnedMutexGuard<()>>,
}

impl Turn {
    /// For an item (`top`) going to `dest`: the space it leaves only when it moves. None to take when the caller holds
    /// the spaces' locks for the whole change already (`held`).
    pub(super) fn new(st: &AppState, top: &Node, dest: &Node, moving: bool, held: bool) -> Turn {
        if held {
            return Turn { st: st.clone(), drives: Vec::new(), held: Vec::new() };
        }
        let mut drives: Vec<String> = [(moving && top.in_folder_space()).then(|| top.drive()), dest.in_folder_space().then(|| dest.drive())]
            .into_iter()
            .flatten()
            .map(str::to_string)
            .collect();
        drives.sort();
        drives.dedup();
        Turn { st: st.clone(), drives, held: Vec::new() }
    }

    pub(super) async fn take(&mut self) {
        if self.held.is_empty() {
            for d in &self.drives {
                self.held.push(lock_space(&self.st, d).await);
            }
        }
    }

    pub(super) fn release(&mut self) {
        self.held.clear();
    }
}

/// Each item's path relative to the first one (the item being moved or copied); the list is ordered by depth
pub(super) fn layout(nodes: &[Node]) -> Vec<Option<String>> {
    let mut rels: HashMap<&str, String> = HashMap::new();
    let mut out = Vec::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate() {
        let rel = if i == 0 { Some(String::new()) } else { n.parent_id.as_deref().and_then(|p| rels.get(p)).map(|p| child_rel(p, &n.name)) };
        if let Some(r) = &rel {
            rels.insert(n.id.as_str(), r.clone());
        }
        out.push(rel);
    }
    out
}

/// Writes the items to `top` on disk, from wherever their content is
pub(super) async fn write_tree(st: &AppState, nodes: &[Node], top: &Pinned, progress: &Tracker) -> AppResult<()> {
    for (n, rel) in nodes.iter().zip(layout(nodes)) {
        let Some(rel) = rel else { continue };
        let to = below(top, &rel).map_err(disk_error)?;
        if n.is_folder() {
            tokio::fs::create_dir(to.as_path()).await.map_err(disk_error)?;
            progress.add(ITEM_WORK);
            continue;
        }
        match Source::resolve(st, n).await? {
            Source::File(from) => {
                let copied = tokio::task::spawn_blocking(move || {
                    let want = std::fs::symlink_metadata(from.as_path())?.len();
                    crate::beneath::copy_file(&from, to.as_path()).map(|got| got == want)
                })
                .await?;
                if !copied.map_err(disk_error)? {
                    return Err(incomplete());
                }
            }
            source => {
                let mut reader =
                    source.open(st, 0, n.size as u64).await.map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, format!("Couldn't read \"{}\": {e}", n.name)))?;
                let mut file = tokio::fs::OpenOptions::new().write(true).create_new(true).open(to.as_path()).await.map_err(disk_error)?;
                let got = tokio::io::copy(&mut reader, &mut file).await.map_err(disk_error)?;
                file.sync_all().await.map_err(disk_error)?;
                if got != n.size as u64 {
                    return Err(incomplete());
                }
            }
        }
        progress.add(work_of(n));
    }
    Ok(())
}

pub(super) const CHANGED_WHILE_COPIED: &str = "the file changed while it was being copied";

/// Copies a file of a folder space to `to`; returns its size and modification time as it was copied (it mustn't change
/// meanwhile)
pub(super) fn copy_checked(from: &Pinned, to: &Path) -> io::Result<(u64, i64)> {
    let mut src = from.open_file()?;
    let before = src.metadata()?;
    let mut dst = std::fs::File::create_new(to)?;
    let n = io::copy(&mut src, &mut dst)?;
    dst.sync_all()?;
    let after = std::fs::symlink_metadata(from.as_path())?;
    let mtime = crate::folders::mtime_ns(&before);
    if n != before.len() || after.len() != before.len() || crate::folders::mtime_ns(&after) != mtime {
        return Err(crate::hashing::unusable(crate::hashing::Unusable::Changed, CHANGED_WHILE_COPIED));
    }
    Ok((n, mtime))
}

/// Stores the files of a folder space in the content store of the space `drive`, for a move (`moving`) or a copy
pub(super) async fn ingest(st: &AppState, nodes: &[Node], drive: &str, moving: bool, progress: &Tracker) -> AppResult<Placed> {
    let (mut staged, mut seen) = (HashMap::new(), HashMap::new());
    for n in nodes.iter().filter(|n| !n.is_folder()) {
        let tmp = st.tmp_dir().join(new_id());
        let stored = async {
            let (from, to) = (abs(n)?, tmp.clone());
            let copied = tokio::task::spawn_blocking(move || copy_checked(&from, &to)).await?.map_err(|e| {
                if crate::hashing::unusable_kind(&e) != Some(crate::hashing::Unusable::Changed) {
                    disk_error(e)
                } else if moving {
                    AppError::conflict(format!("\"{}\" changed while it was being moved. Try again.", n.name))
                } else {
                    AppError::conflict(format!("\"{}\" changed while it was being copied. Try again.", n.name))
                }
            })?;
            let (hash, size) = crate::files::hash_file(tmp.clone()).await?;
            Ok::<_, AppError>((tree::stage_blob(st, drive, hash, size as i64, tmp.clone()).await?, copied))
        }
        .await;
        match stored {
            Ok((s, copied)) => {
                progress.add(work_of(n));
                staged.insert(n.id.clone(), s);
                seen.insert(n.id.clone(), copied);
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                for (_, s) in staged {
                    tree::abandon_staged(st, s).await;
                }
                return Err(e);
            }
        }
    }
    progress.add(ITEM_WORK * nodes.iter().filter(|n| n.is_folder()).count() as u64);
    Ok(Placed::Store(staged, seen))
}

/// Puts the content of `nodes` (an item and everything in it) into place for `dest`. A move within the same disk renames
/// the item away from its folder with `turn` held, and keeps it held; copying or writing content lets it go.
pub(super) async fn place(st: &AppState, nodes: &[Node], dest: &Node, moving: bool, progress: &Tracker, turn: &mut Turn) -> AppResult<Placed> {
    let top = &nodes[0];
    if !dest.in_folder_space() {
        turn.release();
        return ingest(st, nodes, dest.drive(), moving, progress).await;
    }
    for n in nodes {
        check_name(&n.name)?;
    }
    // The first steps on disk: a folder for the item under a name scans ignore, and for a move within the same disk,
    // the item renamed into it
    let (d, t, rename_first, other) = (dest.clone(), top.clone(), moving && top.in_folder_space(), other_disk());
    if rename_first {
        turn.take().await;
    } else {
        turn.release();
    }
    let (within, wrap, tmp, renamed) = on_disk(dest.drive(), disk_wake(), move || {
        let within = abs(&d)?;
        let wrap = within.join(&format!("{}{}", if moving { MOVE_PREFIX } else { COPY_PREFIX }, new_id())).map_err(gone_or_disk_error)?;
        std::fs::create_dir(wrap.as_path()).map_err(disk_error)?;
        let tmp = wrap.join(&t.name).map_err(disk_error)?;
        if !rename_first {
            return Ok((within, wrap, tmp, None));
        }
        let from = abs(&t)?;
        let renamed = if other { Err(io::ErrorKind::CrossesDevices.into()) } else { std::fs::rename(from.as_path(), tmp.as_path()) };
        match renamed {
            Ok(()) => Ok((within, wrap, tmp, Some(Ok(from)))),
            Err(e) if e.kind() != io::ErrorKind::CrossesDevices => {
                let _ = std::fs::remove_dir(wrap.as_path());
                Err(disk_error(e))
            }
            // Another disk
            Err(_) => Ok((within, wrap, tmp, Some(Err(from)))),
        }
    })
    .await?;
    match renamed {
        Some(Ok(from)) => {
            progress.add(nodes.iter().map(work_of).sum());
            return Ok(Placed::Disk { wrap, tmp, renamed_from: Some(from), copied: None, placed: Default::default() });
        }
        // Another disk: copy everything, including what the index doesn't have yet, before the original goes. Until the
        // index follows, the original stays where it was and this is only a copy: it is made in a folder named as one,
        // which is removed rather than put back should ThirtyFile stop meanwhile (`clean_leftovers`)
        Some(Err(from)) => {
            turn.release();
            let (name, p) = (top.name.clone(), progress.clone());
            let copied = tokio::task::spawn_blocking(move || {
                let copy_wrap = within.join(&format!("{COPY_PREFIX}{}", new_id())).map_err(|e| (e, None))?;
                if let Err(e) = rename_new(wrap.as_path(), copy_wrap.as_path()) {
                    let _ = std::fs::remove_dir(wrap.as_path());
                    return Err((e, None));
                }
                let tmp = copy_wrap.join(&name).map_err(|e| (e, Some(copy_wrap.clone())))?;
                let mut items = Vec::new();
                match copy_tree(&from, &tmp, "", &mut items, &p) {
                    Ok(()) => Ok((copy_wrap, tmp, CopiedTree { top: from, items })),
                    Err(e) => Err((e, Some(copy_wrap))),
                }
            })
            .await?;
            return match copied {
                Ok((wrap, tmp, copied)) => Ok(Placed::Disk { wrap, tmp, renamed_from: None, copied: Some(copied), placed: Default::default() }),
                Err((e, wrap)) => {
                    remove_later(wrap.into_iter().collect());
                    Err(disk_error(e))
                }
            };
        }
        None => {}
    }
    if let Err(e) = write_tree(st, nodes, &tmp, progress).await {
        remove_later(vec![wrap]);
        return Err(e);
    }
    Ok(Placed::Disk { wrap, tmp, renamed_from: None, copied: None, placed: Default::default() })
}

/// A move or copy's progress counts each item as this many bytes besides its content: making a file or folder takes
/// about as long as copying that much, so a folder of many small files doesn't look done long before it is
pub(super) const ITEM_WORK: u64 = 256 * 1024;

/// What moving or copying an item counts for in the progress
pub(super) fn work_of(n: &Node) -> u64 {
    ITEM_WORK + if n.is_folder() { 0 } else { n.size.max(0) as u64 }
}

/// Folders holding an item on its way into a folder (`Placed::Disk`)
pub const MOVE_PREFIX: &str = ".thirtyfile-move-";
pub(crate) const COPY_PREFIX: &str = ".thirtyfile-copy-";

/// Takes back content put in place when the index couldn't follow
pub(super) async fn undo(st: &AppState, placed: Placed) {
    match placed {
        Placed::Disk { wrap, tmp, renamed_from, placed, .. } => {
            let placed = (*placed).into_inner();
            // On a blocking thread (the disk may be one that doesn't answer), waited for so that it is back when the
            // change reports its failure
            let back = tokio::task::spawn_blocking(move || {
                // Already under its name in the destination: back into the folder it was put in first
                if let Some(at) = placed
                    && let Err(e) = rename_new(at.as_path(), tmp.as_path())
                {
                    tracing::warn!("Couldn't take {:?} back from where it was put: {e}", at.name().unwrap_or_default());
                    return;
                }
                match renamed_from {
                    // Back under its name, or under a numbered one should something have taken that meanwhile (never
                    // over it)
                    Some(from) => {
                        let is_dir = std::fs::symlink_metadata(tmp.as_path()).is_ok_and(|m| m.is_dir());
                        let back = match (from.parent(), from.name()) {
                            (Some(dir), Some(name)) => {
                                put_back_into(&tmp, &dir, name, is_dir).unwrap_or_else(|| Err(io::ErrorKind::AlreadyExists.into())).map(|_| ())
                            }
                            _ => rename_new(tmp.as_path(), from.as_path()),
                        };
                        match back {
                            Ok(()) => {
                                let _ = std::fs::remove_dir(wrap.as_path());
                            }
                            Err(e) => tracing::warn!("Couldn't move {:?} back where it was: {e}", from.name().unwrap_or_default()),
                        }
                    }
                    // A copy: it goes
                    None => remove_later(vec![wrap]),
                }
            });
            let _ = tokio::time::timeout(disk_wait(), back).await;
        }
        Placed::Store(staged, _) => {
            for (_, s) in staged {
                tree::abandon_staged(st, s).await;
            }
        }
    }
}

/// After the index changed: staged content is kept, content no longer used goes
pub(super) async fn finish(st: &AppState, placed: Placed, extras: Vec<BlobRef>) {
    if let Placed::Store(staged, _) = placed {
        for (_, s) in staged {
            tree::finish_staged(st, s, None).await;
        }
    }
    tree::schedule_blob_removal(st, extras);
}

/// Names for the items going into the content store, where letter case doesn't tell names apart: "A.txt" and "a.txt"
/// from a folder can't both keep theirs. The first item's name is `top_name`.
pub(super) fn store_names(nodes: &[Node], top_name: &str) -> Vec<String> {
    let mut taken: HashMap<&str, HashSet<String>> = HashMap::new();
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            if i == 0 {
                return top_name.to_string();
            }
            let set = taken.entry(n.parent_id.as_deref().unwrap_or_default()).or_default();
            let mut name = n.name.clone();
            let mut k = 1;
            while !set.insert(name.to_lowercase()) {
                name = numbered_name(&n.name, k, n.is_folder());
                k += 1;
            }
            name
        })
        .collect()
}

pub(super) fn id_list<'a>(ids: impl Iterator<Item = &'a str>) -> String {
    serde_json::to_string(&ids.collect::<Vec<_>>()).unwrap()
}

/// Items to move or copy to or from a folder space, checked in the transaction of the move or copy. Their content is
/// copied (or renamed) item by item afterwards, which can take long: the request runs it as a job (jobs.rs).
pub struct Across {
    pub(super) dest: Node,
    /// Each item with everything in it (not in the trash), ordered by depth
    pub(super) items: Vec<Vec<Node>>,
    pub(super) moving: bool,
    /// Items of the destination these replace, moved to the trash in the change's transaction already
    pub(super) replaced: Vec<String>,
    /// Whether the caller holds the scan locks of the spaces involved for the whole change
    pub(super) held: bool,
}

impl Across {
    /// None when there is nothing to do this way
    pub fn new(dest: Node, items: Vec<Vec<Node>>, moving: bool) -> Option<Across> {
        (!items.is_empty()).then_some(Across { dest, items, moving, replaced: Vec::new(), held: false })
    }

    /// The items of the destination (ids) the items replace: put in the trash so these can take their names
    pub fn replacing(mut self, replaced: Vec<String>) -> Across {
        self.replaced = replaced;
        self
    }

    /// The caller holds the scan locks of both spaces until the change is done (removing a personal space)
    pub fn spaces_held(mut self) -> Across {
        self.held = true;
        self
    }

    pub fn dest(&self) -> &Node {
        &self.dest
    }

    pub fn replaced(&self) -> &[String] {
        &self.replaced
    }

    pub async fn run(self, st: &AppState, user: &User, progress: &Tracker) -> AppResult<()> {
        progress.add_total(self.items.iter().flatten().map(work_of).sum());
        if self.moving {
            move_items_across(st, user, &self.dest, self.items, progress, self.held).await
        } else {
            copy_items_across(st, user, &self.dest, self.items, progress, self.held).await
        }
    }
}

/// Moves items to a folder of another space, where one of the two (or both) is a folder space. The content is put in
/// place first (renamed when both folders are on the same disk, else copied); then the index moves the items, which
/// keep their ids and with them their shares, permissions and favourites; then the originals are removed.
/// `items`: each item with everything in it (not in the trash), ordered by depth. `held`: the caller holds the scan
/// locks of both spaces for the whole change.
async fn move_items_across(st: &AppState, user: &User, dest: &Node, items: Vec<Vec<Node>>, progress: &Tracker, held: bool) -> AppResult<()> {
    for nodes in items {
        let mut turn = Turn::new(st, &nodes[0], dest, true, held);
        let mut placed = place(st, &nodes, dest, true, progress, &mut turn).await?;
        #[cfg(test)]
        after_place().await;
        turn.take().await;
        let result = {
            let _w = st.write_lock.lock().await;
            commit_move(st, user, dest, &nodes, &placed, held).await
        };
        match result {
            Ok((extras, remove)) => {
                placed_for_good(&placed).await;
                let copied = match &mut placed {
                    Placed::Disk { copied, .. } => copied.take(),
                    Placed::Store(..) => None,
                };
                finish(st, placed, extras).await;
                if let Some(copied) = copied {
                    tokio::task::spawn_blocking(move || remove_copied(copied));
                }
                // Stored in the content store: the originals go, each file only if it is still what was stored
                if !remove.is_empty() {
                    let from = nodes[0].clone();
                    tokio::task::spawn_blocking(move || {
                        if let Ok(top) = space_root(&from) {
                            remove_copied(CopiedTree { top, items: remove });
                        }
                    });
                }
            }
            Err(e) => {
                undo(st, placed).await;
                return Err(e);
            }
        }
        drop(turn);
    }
    Ok(())
}

/// Checks, in the transaction that records a move or copy, that the destination is still as the content was put in
/// place for: there, writable, and in the same space kept the same way (its folder, or the content store). The space
/// may have been moved to another storage location, or made read-only, while the content was copied.
pub(super) async fn still_there(conn: &mut SqliteConnection, dest: &Node) -> AppResult<()> {
    let now =
        tree::get_node(conn, &dest.id).await?.filter(|d| d.trashed_at.is_none()).ok_or_else(|| AppError::not_found("The destination folder no longer exists"))?;
    if now.space_read_only {
        return Err(tree::read_only_error(&now));
    }
    if now.drive_id != dest.drive_id || now.fs_root != dest.fs_root {
        return Err(AppError::conflict("The destination was moved to another storage location meanwhile. Try again."));
    }
    Ok(())
}

/// Renames content put in place for `dest` (`Placed::Disk`) to `name` in it, noting where in `placed` (the folder that
/// held it stays until the change is committed, should `undo` need to take it back); returns what is at each of `rels`
/// (paths below it, from `layout`) now, None where nothing is
pub(super) async fn put_in_place(
    dest: &Node,
    tmp: &Pinned,
    placed: &crate::sync::Mutex<Option<Pinned>>,
    name: &str,
    rels: &[Option<String>],
) -> AppResult<Vec<Option<Stat>>> {
    let (d, tmp, name, rels) = (dest.clone(), tmp.clone(), name.to_string(), rels.to_vec());
    let (final_path, stats) = on_disk(dest.drive(), disk_wait(), move || {
        let final_path = abs(&d)?.join(&name).map_err(gone_or_disk_error)?;
        rename_new(tmp.as_path(), final_path.as_path()).map_err(disk_error)?;
        let stats = rels.iter().map(|rel| rel.as_ref().and_then(|rel| below(&final_path, rel).and_then(|p| stat(p.as_path())).ok())).collect();
        Ok((final_path, stats))
    })
    .await?;
    *placed.lock() = Some(final_path);
    #[cfg(test)]
    if testing::stops(dest.drive(), testing::Stop::Placed) {
        return Err(testing::stopped());
    }
    Ok(stats)
}

/// Once the index followed content put in place: the folder that held it goes, before the change is reported done
pub(super) async fn placed_for_good(placed: &Placed) {
    if let Placed::Disk { wrap, placed, .. } = placed
        && placed.lock().is_some()
    {
        let wrap = wrap.clone();
        let removed = tokio::task::spawn_blocking(move || {
            let _ = std::fs::remove_dir(wrap.as_path());
        });
        let _ = tokio::time::timeout(disk_wait(), removed).await;
    }
}

async fn authorize_move(conn: &mut SqliteConnection, user: &User, dest: &Node, top: &Node, held: bool) -> AppResult<()> {
    // Account removal explicitly preserves a personal space for an administrator with both scan locks held and
    // its moving flag set. Ordinary file operations and protocol actors never enter this maintenance path.
    let administrative = held
        && user.is_admin()
        && user.protocol_root.is_none()
        && user.sftp_credential.is_none()
        && tree::get_drive(conn, top.drive()).await?.is_some_and(|d| d.kind == tree::SpaceKind::Personal && d.moving);
    if administrative {
        let current = crate::auth::require_admin(conn, user).await?;
        crate::drives::manageable_drive(conn, &current, dest.drive()).await?;
    } else {
        tree::node_for(conn, user, &dest.id, tree::Need::Add).await?;
        tree::node_for(conn, user, &top.id, tree::Need::Move).await?;
        tree::require_subtree(conn, user, top, tree::Need::Move).await?;
    }
    Ok(())
}

/// The index side of a move; returns content no longer used and (for items now in the content store) what to remove
/// from disk (paths below the space's folder, folders before what is in them)
pub(super) async fn commit_move(st: &AppState, user: &User, dest: &Node, nodes: &[Node], placed: &Placed, held: bool) -> AppResult<(Vec<BlobRef>, Vec<Copied>)> {
    let mut tx = crate::db::begin_write(&st.db).await?;
    let top = &nodes[0];
    authorize_move(&mut tx, user, dest, top, held).await?;
    // Everything must still be as it was when the content was copied: items added to a folder of the content store
    // meanwhile would be left behind
    let changed = || AppError::conflict(format!("\"{}\" changed while it was being moved. Try again.", top.name));
    let current = tree::get_node(&mut tx, &top.id).await?.ok_or_else(changed)?;
    if current.trashed_at.is_some() || current.parent_id != top.parent_id || current.drive_id != top.drive_id || current.fs_root != top.fs_root {
        return Err(changed());
    }
    // The space it leaves may have started moving to another storage location while the content was copied: that move
    // has its items as they are, and switches them over itself. (A personal space being removed is read-only too, while
    // its files are moved out this way.)
    if current.space_moving && crate::moves::drive_busy(&mut tx, current.drive()).await? {
        return Err(tree::read_only_error(&current));
    }
    still_there(&mut tx, dest).await?;
    let planned: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let now_there = tree::live_subtree_ids(&mut tx, &top.id).await?;
    if now_there.len() != planned.len() || !now_there.iter().all(|id| planned.contains(id.as_str())) {
        return Err(changed());
    }
    let ids = id_list(nodes.iter().map(|n| n.id.as_str()));
    let src_drive = top.drive().to_string();
    let src_root = tree::get_drive(&mut tx, &src_drive).await?.map(|d| d.root_id).unwrap_or_default();
    // Items in the trash stay in the trash of their space
    sqlx::query("UPDATE nodes SET parent_id = ? WHERE trash_root = 1 AND parent_id IN (SELECT value FROM json_each(?))")
        .bind(&src_root)
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    // What the space it leaves stops counting (as the index counted it), and what the destination counts now (as the
    // content found or stored): items gone meanwhile are purged, which takes them off the space's count itself
    let (mut leaves, mut arrives) = (0i64, 0i64);
    let mut extras = Vec::new();
    let mut remove = Vec::new();
    match placed {
        Placed::Disk { tmp, placed, .. } => {
            let rels = layout(nodes);
            let stats = put_in_place(dest, tmp, placed, &top.name, &rels).await?;
            let dest_rel = child_rel(rel_of(dest), &top.name);
            for ((n, rel), s) in nodes.iter().zip(rels).zip(stats) {
                let Some(rel) = rel else { continue };
                let full = if rel.is_empty() { dest_rel.clone() } else { format!("{dest_rel}/{rel}") };
                match s {
                    Some(s) => {
                        record(&mut tx, &n.id, dest.drive(), &full, &s).await?;
                        if !n.is_folder() {
                            leaves += n.size;
                            arrives += s.size;
                        }
                    }
                    // Gone from the server before it could be moved: gone from the index too, with the earlier versions
                    // it may still have in the content store
                    None => extras.extend(tree::purge_subtree(&mut tx, &n.id).await?),
                }
            }
            // Content that was in the content store is in the folder now
            let hashes: Vec<String> = nodes.iter().filter_map(|n| n.blob_hash.clone()).collect();
            sqlx::query("UPDATE nodes SET blob_hash = NULL WHERE id IN (SELECT value FROM json_each(?))").bind(&ids).execute(&mut *tx).await?;
            extras.extend(tree::release_blobs(&mut tx, &hashes).await?);
        }
        Placed::Store(staged, seen) => {
            let names = store_names(nodes, &top.name);
            // Leave the names the folder gave them first, so renamed items can't run into each other on the way
            sqlx::query("UPDATE nodes SET name = char(1) || id WHERE id IN (SELECT value FROM json_each(?))").bind(&ids).execute(&mut *tx).await?;
            sqlx::query(
                "UPDATE nodes SET drive_id = ?, fs_path = NULL, fs_dev = NULL, fs_ino = NULL, fs_size = NULL, fs_mtime_ns = NULL, fs_birth_ns = NULL
                 WHERE id IN (SELECT value FROM json_each(?))",
            )
            .bind(dest.drive())
            .bind(&ids)
            .execute(&mut *tx)
            .await?;
            for (n, name) in nodes.iter().zip(&names) {
                sqlx::query("UPDATE nodes SET name = ? WHERE id = ?").bind(name).bind(&n.id).execute(&mut *tx).await?;
                if !n.is_folder() {
                    leaves += n.size;
                    arrives += staged.get(&n.id).map_or(n.size, |s| s.size);
                }
                if let Some(s) = staged.get(&n.id) {
                    extras.extend(tree::commit_blob(&mut tx, s).await?);
                    sqlx::query("UPDATE nodes SET blob_hash = ?, size = ? WHERE id = ?").bind(&s.hash).bind(s.size).bind(&n.id).execute(&mut *tx).await?;
                }
                if let Some(rel) = n.fs_path.clone().filter(|r| !r.is_empty()) {
                    remove.push(Copied { rel, is_dir: n.is_folder(), seen: seen.get(&n.id).copied() });
                }
            }
        }
    }
    sqlx::query("UPDATE nodes SET parent_id = ? WHERE id = ?").bind(&dest.id).bind(&top.id).execute(&mut *tx).await?;
    tree::adjust_usage(&mut tx, &src_drive, -leaves).await?;
    tree::adjust_usage(&mut tx, dest.drive(), arrives).await?;
    if let Some(p) = &top.parent_id {
        tree::touch(&mut tx, p).await?;
    }
    tree::touch(&mut tx, &dest.id).await?;
    let to = tree::place_name(&mut tx, dest).await?;
    logs::record_activity(&mut tx, user, Some(top), "move", &format!("→ {to}")).await?;
    tx.commit().await?;
    Ok((extras, remove))
}

/// Copies items to a folder of another space (or of the same folder space), where one of the two (or both) is a folder
/// space: the content is written first, then indexed. `plans`: each item with everything in it (not in the trash),
/// ordered by depth. `held` as for moves.
async fn copy_items_across(st: &AppState, user: &User, dest: &Node, plans: Vec<Vec<Node>>, progress: &Tracker, held: bool) -> AppResult<()> {
    for nodes in plans {
        let mut turn = Turn::new(st, &nodes[0], dest, false, held);
        let placed = place(st, &nodes, dest, false, progress, &mut turn).await?;
        #[cfg(test)]
        after_place().await;
        turn.take().await;
        let result = {
            let _w = st.write_lock.lock().await;
            commit_copy(st, user, dest, &nodes, &placed).await
        };
        match result {
            Ok(extras) => {
                placed_for_good(&placed).await;
                finish(st, placed, extras).await;
            }
            Err(e) => {
                undo(st, placed).await;
                return Err(e);
            }
        }
        drop(turn);
    }
    Ok(())
}

pub(super) async fn commit_copy(st: &AppState, user: &User, dest: &Node, nodes: &[Node], placed: &Placed) -> AppResult<Vec<BlobRef>> {
    let mut tx = crate::db::begin_write(&st.db).await?;
    let top = &nodes[0];
    tree::node_for(&mut tx, user, &dest.id, tree::Need::Add).await?;
    tree::node_for(&mut tx, user, &top.id, tree::Need::Read).await?;
    tree::require_subtree(&mut tx, user, top, tree::Need::Read).await?;
    // A restriction may have been added while bytes were copied. Refuse the old plan rather than publishing a
    // newly restricted item under a copy made from stale metadata.
    for n in nodes {
        let current = tree::node_for(&mut tx, user, &n.id, tree::Need::Read).await?;
        if current.restricted != n.restricted {
            return Err(AppError::conflict("Access changed while the folder was being copied. Try again."));
        }
    }
    still_there(&mut tx, dest).await?;
    let mut ids: HashMap<&str, String> = HashMap::new();
    let mut extras = Vec::new();
    let mut bytes = 0i64;
    match placed {
        Placed::Disk { tmp, placed, .. } => {
            let name = free_name(&mut tx, dest, &top.name, top.is_folder()).await?;
            let rels = layout(nodes);
            let stats = put_in_place(dest, tmp, placed, &name, &rels).await?;
            let dest_rel = child_rel(rel_of(dest), &name);
            for (i, ((n, rel), s)) in nodes.iter().zip(rels).zip(stats).enumerate() {
                let Some(rel) = rel else { continue };
                let parent = if i == 0 { dest.id.clone() } else { ids.get(n.parent_id.as_deref().unwrap_or_default()).cloned().unwrap_or_default() };
                let Some(s) = s else { continue };
                if parent.is_empty() {
                    continue;
                }
                let id = new_id();
                let full = if rel.is_empty() { dest_rel.clone() } else { format!("{dest_rel}/{rel}") };
                insert_at(&mut tx, &id, user.id, At { parent: &parent, drive: dest.drive(), name: if i == 0 { &name } else { &n.name }, rel: &full }, &s).await?;
                sqlx::query("UPDATE nodes SET restricted = ? WHERE id = ?").bind(n.restricted).bind(&id).execute(&mut *tx).await?;
                bytes += s.size;
                ids.insert(n.id.as_str(), id);
            }
        }
        Placed::Store(staged, _) => {
            let name = tree::unique_name(&mut tx, &dest.id, &top.name, top.is_folder()).await?;
            let names = store_names(nodes, &name);
            let ts = now();
            for (i, (n, name)) in nodes.iter().zip(&names).enumerate() {
                let parent = if i == 0 { dest.id.clone() } else { ids.get(n.parent_id.as_deref().unwrap_or_default()).cloned().unwrap_or_default() };
                if parent.is_empty() {
                    continue;
                }
                let s = staged.get(&n.id);
                if let Some(s) = s {
                    extras.extend(tree::commit_blob(&mut tx, s).await?);
                    bytes += s.size;
                }
                let id = new_id();
                sqlx::query(
                    "INSERT INTO nodes (id, owner_id, parent_id, kind, name, blob_hash, size, mime, drive_id, created_at, updated_at, restricted)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(user.id)
                .bind(&parent)
                .bind(&n.kind)
                .bind(name)
                .bind(s.map(|s| s.hash.clone()))
                .bind(s.map(|s| s.size).unwrap_or(0))
                .bind(&n.mime)
                .bind(dest.drive())
                .bind(ts)
                .bind(ts)
                .bind(n.restricted)
                .execute(&mut *tx)
                .await?;
                ids.insert(n.id.as_str(), id);
            }
        }
    }
    // The copies get the copier's tags of the originals (each person's tags are their own)
    crate::tags::copy_tags(&mut tx, user.id, ids.iter().map(|(from, to)| (*from, to.as_str()))).await?;
    tree::adjust_usage(&mut tx, dest.drive(), bytes).await?;
    tree::touch(&mut tx, &dest.id).await?;
    let to = tree::place_name(&mut tx, dest).await?;
    logs::record_activity(&mut tx, user, Some(top), "copy", &format!("→ {to}")).await?;
    tx.commit().await?;
    Ok(extras)
}

/// Tests: a move across spaces, as `Across::run` makes it
#[cfg(test)]
pub async fn move_across(st: &AppState, user: &User, dest: &Node, items: Vec<Vec<Node>>, progress: &Tracker) -> AppResult<()> {
    move_items_across(st, user, dest, items, progress, false).await
}

/// Tests: a copy across spaces, as `Across::run` makes it
#[cfg(test)]
pub async fn copy_across(st: &AppState, user: &User, dest: &Node, plans: Vec<Vec<Node>>, progress: &Tracker) -> AppResult<()> {
    copy_items_across(st, user, dest, plans, progress, false).await
}

#[cfg(test)]
mod tests {
    use super::super::testing::{self, Stop};
    use crate::testutil::{self, write_old};
    use axum::{Json, extract::State};

    #[tokio::test]
    async fn a_move_or_copy_that_fails_once_its_content_is_in_place_takes_it_back() {
        for moving in [true, false] {
            let env = testutil::env().await;
            let one = env.folder_space("One").await;
            let two = env.folder_space("Two").await;
            let admin = env.admin().await;
            write_old(&one.dir.join("Docs/a.txt"), b"alpha");
            crate::folders::scan(&env.st, &one.drive).await.unwrap();
            let (docs, _) = env.node_at(&one.drive, "Docs").await.unwrap();
            let _stop = testing::stop_at(&two.drive, Stop::Placed);
            let req = || Json(serde_json::from_value(serde_json::json!({ "ids": [docs], "dest_id": two.root })).unwrap());
            let res = if moving {
                crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req()).await
            } else {
                crate::nodes::copy_nodes(State(env.st.clone()), admin.clone(), req()).await
            };
            // Failed at once, or as a job that went on after the answer
            if let Ok(Json(job)) = res {
                assert_eq!(crate::jobs::wait_for(&env.st, &job.id).await.state, "failed");
            }

            // The index didn't follow, so neither does the folder: the item is only where it was
            assert!(!two.dir.join("Docs").exists(), "moving: {moving}");
            assert_eq!(std::fs::read(one.dir.join("Docs/a.txt")).unwrap(), b"alpha");
            assert_eq!(env.node_at(&one.drive, "Docs").await.map(|n| n.0), Some(docs.clone()));
            let r = crate::folders::scan(&env.st, &two.drive).await.unwrap();
            assert_eq!(r.added, 0, "moving: {moving}");
        }
    }

    #[tokio::test]
    async fn an_item_to_be_replaced_comes_back_when_the_move_or_copy_fails() {
        for moving in [true, false] {
            let env = testutil::env().await;
            let one = env.folder_space("One").await;
            let two = env.folder_space("Two").await;
            let admin = env.admin().await;
            write_old(&one.dir.join("Docs/a.txt"), b"alpha");
            write_old(&two.dir.join("Docs/b.txt"), b"bravo");
            crate::folders::scan(&env.st, &one.drive).await.unwrap();
            crate::folders::scan(&env.st, &two.drive).await.unwrap();
            let (docs, _) = env.node_at(&one.drive, "Docs").await.unwrap();
            let (there, _) = env.node_at(&two.drive, "Docs").await.unwrap();
            let _stop = testing::stop_at(&two.drive, Stop::Placed);
            let req = || {
                let v = serde_json::json!({ "ids": [docs], "dest_id": two.root, "resolutions": { docs.clone(): "replace" } });
                Json(serde_json::from_value(v).unwrap())
            };
            let res = if moving {
                crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req()).await
            } else {
                crate::nodes::copy_nodes(State(env.st.clone()), admin.clone(), req()).await
            };
            if let Ok(Json(job)) = res {
                assert_eq!(crate::jobs::wait_for(&env.st, &job.id).await.state, "failed");
            }

            // Asked to replace, it didn't: what was there is back where it was, out of the trash
            assert_eq!(env.node_at(&two.drive, "Docs").await.map(|n| n.0), Some(there.clone()), "moving: {moving}");
            assert_eq!(std::fs::read(two.dir.join("Docs/b.txt")).unwrap(), b"bravo", "moving: {moving}");
            assert!(!two.dir.join("Docs/a.txt").exists(), "moving: {moving}");
            assert_eq!(std::fs::read(one.dir.join("Docs/a.txt")).unwrap(), b"alpha");
        }
    }
}
