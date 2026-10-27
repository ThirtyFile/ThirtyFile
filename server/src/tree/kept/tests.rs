use super::*;
use crate::{auth::User, testutil};

const DAY: i64 = 86400;

struct World {
    env: testutil::TestEnv,
    amy: User,
    /// A folder with a file in it, and another file at the top
    folder: String,
    inside: String,
    outside: String,
}

async fn world() -> World {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let root = amy.root_id.clone().unwrap();
    let folder = env.folder(&amy, &root, "Cases").await;
    let inside = env.file(&amy, &folder, "contract.txt").await;
    let outside = env.file(&amy, &root, "notes.txt").await;
    World { env, amy, folder, inside, outside }
}

impl World {
    async fn hold(&self, scope: &str, node: Option<&str>, drive: Option<&str>, user: Option<i64>, reason: &str) -> String {
        let id = crate::util::new_id();
        sqlx::query(
            "INSERT INTO holds (id, scope, node_id, drive_id, user_id, label, reason, created_by_name, created_at) VALUES (?, ?, ?, ?, ?, 'x', ?, 'admin', ?)",
        )
        .bind(&id)
        .bind(scope)
        .bind(node)
        .bind(drive)
        .bind(user)
        .bind(reason)
        .bind(now())
        .execute(&self.env.st.db)
        .await
        .unwrap();
        id
    }

    async fn release(&self, id: &str) {
        sqlx::query("UPDATE holds SET released_at = ?, released_by_name = 'admin' WHERE id = ?").bind(now()).bind(id).execute(&self.env.st.db).await.unwrap();
    }

    async fn drive(&self) -> String {
        self.env.drive_of(&self.folder).await
    }

    async fn retention(&self, days: i64) {
        let drive = self.drive().await;
        sqlx::query("UPDATE drives SET retention_days = ? WHERE id = ?").bind(days).bind(drive).execute(&self.env.st.db).await.unwrap();
    }

    async fn changed(&self, id: &str, days_ago: i64) {
        sqlx::query("UPDATE nodes SET updated_at = ? WHERE id = ?").bind(now() - days_ago * DAY).bind(id).execute(&self.env.st.db).await.unwrap();
    }

    async fn subtrees(&self, ids: &[&str]) -> HashMap<String, Kept> {
        let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        subtrees(&mut self.env.st.db.acquire().await.unwrap(), &ids).await.unwrap()
    }

    async fn nodes(&self, ids: &[&str]) -> HashMap<String, Kept> {
        let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        nodes(&mut self.env.st.db.acquire().await.unwrap(), &ids).await.unwrap()
    }

    async fn forced(&self) -> bool {
        in_force(&mut self.env.st.db.acquire().await.unwrap()).await.unwrap()
    }

    async fn space_kept(&self) -> bool {
        space_has_kept(&mut self.env.st.db.acquire().await.unwrap(), &self.drive().await).await.unwrap()
    }
}

#[tokio::test]
async fn nothing_is_kept_when_there_is_no_hold_and_no_retention() {
    let w = world().await;
    assert!(!w.forced().await);
    assert!(w.subtrees(&[&w.folder, &w.outside]).await.is_empty());
    assert!(!w.space_kept().await);
}

#[tokio::test]
async fn a_hold_on_a_file_keeps_it_and_the_folders_above_it_but_not_its_neighbours() {
    let w = world().await;
    let other = w.env.file(&w.amy, &w.folder, "other.txt").await;
    w.hold("node", Some(&w.inside), None, None, "Case 12").await;
    let kept = w.subtrees(&[&w.inside, &w.folder, &w.outside, &other]).await;
    assert_eq!(kept[&w.inside].hold.as_deref(), Some("Case 12"));
    // The folder is deleted with what is in it
    assert_eq!(kept[&w.folder].hold.as_deref(), Some("Case 12"));
    assert!(!kept.contains_key(&w.outside) && !kept.contains_key(&other));
    // Looked at on its own, the folder holds nothing back: only the file does
    let own = w.nodes(&[&w.folder, &w.inside]).await;
    assert!(!own.contains_key(&w.folder));
    assert!(own.contains_key(&w.inside));
}

#[tokio::test]
async fn a_hold_on_a_folder_keeps_what_is_in_it_and_what_is_put_in_it_later() {
    let w = world().await;
    w.hold("node", Some(&w.folder), None, None, "Audit").await;
    let later = w.env.file(&w.amy, &w.folder, "later.txt").await;
    let kept = w.nodes(&[&w.inside, &later, &w.outside]).await;
    assert_eq!(kept[&w.inside].hold.as_deref(), Some("Audit"));
    assert_eq!(kept[&later].hold.as_deref(), Some("Audit"));
    assert!(!kept.contains_key(&w.outside));
    // A file of it in the trash on its own is still under the folder's hold
    sqlx::query("UPDATE nodes SET trashed_at = ?, trash_root = 1 WHERE id = ?").bind(now()).bind(&later).execute(&w.env.st.db).await.unwrap();
    assert!(w.subtrees(&[&later]).await.contains_key(&later));
}

#[tokio::test]
async fn a_hold_on_a_space_or_on_a_person_keeps_their_files() {
    let w = world().await;
    let bob = w.env.user("bob", true).await;
    let bobs = w.env.file(&bob, bob.root_id.as_ref().unwrap(), "bobs.txt").await;
    let drive = w.drive().await;
    let space = w.hold("space", None, Some(&drive), None, "Whole space").await;
    let kept = w.subtrees(&[&w.inside, &w.outside, &bobs]).await;
    assert!(kept.contains_key(&w.inside) && kept.contains_key(&w.outside));
    assert!(!kept.contains_key(&bobs), "Bob has a space of his own");
    w.release(&space).await;
    assert!(w.subtrees(&[&w.inside]).await.is_empty());

    // The files a person owns, wherever they are; their folders are not files
    w.hold("user", None, None, Some(w.amy.id), "Amy's files").await;
    let kept = w.subtrees(&[&w.inside, &w.outside, &bobs, &w.folder]).await;
    assert!(kept.contains_key(&w.inside) && kept.contains_key(&w.outside) && kept.contains_key(&w.folder));
    assert!(!kept.contains_key(&bobs));
    assert!(w.nodes(&[&w.folder]).await.is_empty(), "a folder is not a file");
}

#[tokio::test]
async fn a_released_hold_keeps_nothing() {
    let w = world().await;
    let id = w.hold("node", Some(&w.inside), None, None, "Case 12").await;
    assert!(!w.subtrees(&[&w.folder]).await.is_empty());
    w.release(&id).await;
    assert!(w.subtrees(&[&w.folder]).await.is_empty());
    assert!(!w.forced().await);
}

#[tokio::test]
async fn a_retention_period_keeps_a_file_until_it_has_passed_since_its_last_change() {
    let w = world().await;
    w.retention(30).await;
    w.changed(&w.inside, 10).await;
    w.changed(&w.outside, 40).await;
    let kept = w.subtrees(&[&w.inside, &w.outside, &w.folder]).await;
    let until = kept[&w.inside].until.unwrap();
    assert!((until - (now() + 20 * DAY)).abs() < 5);
    // Past its period: free; the folder is kept by the file in it
    assert!(!kept.contains_key(&w.outside));
    assert_eq!(kept[&w.folder].until, Some(until));
    assert!(kept[&w.inside].hold.is_none());
    assert!(w.space_kept().await);
    // The period turned off: nothing is kept
    w.retention(0).await;
    assert!(w.subtrees(&[&w.folder]).await.is_empty());
}

#[tokio::test]
async fn a_trash_item_of_its_own_inside_a_folder_is_not_what_the_folder_keeps() {
    let w = world().await;
    w.retention(30).await;
    w.changed(&w.inside, 40).await;
    let young = w.env.file(&w.amy, &w.folder, "young.txt").await;
    // Put in the trash on its own before the folder: it stays when the folder is deleted, so the folder doesn't wait for it
    sqlx::query("UPDATE nodes SET trashed_at = ?, trash_root = 1, trash_id = 't1' WHERE id = ?").bind(now()).bind(&young).execute(&w.env.st.db).await.unwrap();
    let kept = w.subtrees(&[&w.folder]).await;
    assert!(kept.is_empty(), "{kept:?}");
    // On its own it is kept, as every file inside the period is
    assert!(w.subtrees(&[&young]).await.contains_key(&young));
}

#[tokio::test]
async fn split_tells_the_free_from_the_kept_in_the_order_given() {
    let w = world().await;
    w.hold("node", Some(&w.inside), None, None, "Case 12").await;
    let (free, held) = split(&mut w.env.st.db.acquire().await.unwrap(), vec![w.outside.clone(), w.folder.clone()]).await.unwrap();
    assert_eq!(free, vec![w.outside.clone()]);
    assert_eq!(held.len(), 1);
    assert_eq!((held[0].0.as_str(), held[0].1.hold.as_deref()), (w.folder.as_str(), Some("Case 12")));
}

#[tokio::test]
async fn a_space_with_a_hold_on_something_in_it_is_kept() {
    let w = world().await;
    assert!(!w.space_kept().await);
    let id = w.hold("node", Some(&w.inside), None, None, "Case 12").await;
    assert!(w.space_kept().await);
    w.release(&id).await;
    assert!(!w.space_kept().await);
    w.hold("user", None, None, Some(w.amy.id), "Amy").await;
    assert!(w.space_kept().await);
}
