import { clearDownloads } from "@/downloads";
import { cancelAll } from "@/uploads";
import { forgetRecords, setRecoveryUser } from "@/lib/uploadRecovery";

/** The start of the keys of the tree's expanded folders (earlier versions kept one for everyone, under this key alone) */
const TREE_STORAGE_PREFIX = "tf-tree-expanded";

/** Where the folders a user expanded in the navigation pane are kept (components/FolderTree.tsx) */
export const treeStorageKey = (userId: number) => `${TREE_STORAGE_PREFIX}-${userId}`;

/**
 * Signs out without leaving anything of the user in this browser tab for the next person: transfers stop, the upload
 * resume records, the saved tabs of this user and the expanded folders of the tree are removed, and the sign-in page is loaded afresh, so nothing kept
 * in memory (unsaved drafts, open workbooks, the clipboard, file names in the transfer lists) survives.
 */
export function leaveAfterSignOut(userId: number) {
  cancelAll();
  clearDownloads();
  // Interrupted uploads that could be continued name files and folders too
  forgetRecords(null, true);
  try {
    for (const key of Object.keys(localStorage)) {
      if (key.startsWith("tus::")) localStorage.removeItem(key);
    }
    localStorage.removeItem(`tf-tabs-${userId}`);
    localStorage.removeItem(treeStorageKey(userId));
    sessionStorage.removeItem(`tf-tabs-${userId}`);
  } catch {
    // Storage blocked by the browser: nothing was kept there either
  }
  window.location.assign("/login");
}

/** Who last signed in in this browser (to tell whether what is kept below belongs to someone else) */
const LAST_USER_KEY = "tf-signed-in-user";

/** Keys of `storage` that start with `prefix` */
function keysOf(storage: Storage, prefix: string) {
  return Object.keys(storage).filter((k) => k.startsWith(prefix));
}

/**
 * Removes what is kept in this browser for anyone but `userId`: their saved tabs (whose titles are file and folder
 * names) and the folders they expanded in the tree, and, when someone else signed in here last, the upload resume
 * records (which name files and folders too). Needed when a session ended without signing out (it expired, or the
 * browser was closed), since only signing out clears them (`leaveAfterSignOut`).
 */
function forgetOtherUsers(userId: number) {
  try {
    const ownTabs = `tf-tabs-${userId}`;
    const ownTree = treeStorageKey(userId);
    for (const storage of [localStorage, sessionStorage]) {
      for (const key of keysOf(storage, "tf-tabs-")) if (key !== ownTabs) storage.removeItem(key);
    }
    for (const key of keysOf(localStorage, TREE_STORAGE_PREFIX)) if (key !== ownTree) localStorage.removeItem(key);
    const someoneElse = localStorage.getItem(LAST_USER_KEY) !== String(userId);
    if (someoneElse) {
      for (const key of keysOf(localStorage, "tus::")) localStorage.removeItem(key);
    }
    // Other people's interrupted uploads (and, after someone else, those made through share links in this browser)
    forgetRecords(`u${userId}`, someoneElse);
    localStorage.setItem(LAST_USER_KEY, String(userId));
  } catch {
    // Storage blocked by the browser: nothing was kept there either
  }
}

let signedInAs: number | null = null;

/**
 * Called with the signed-in user whenever it is known. After a session expired, someone else may sign in in the same
 * tab: then the page is loaded afresh, so the previous user's drafts and lists in memory don't reach them. What other
 * users left in the browser's storage (tabs, expanded folders, unfinished uploads) is removed, so the next person
 * doesn't see where they have been.
 */
export function noteSignedIn(userId: number) {
  if (signedInAs !== null && signedInAs !== userId) {
    window.location.reload();
    return;
  }
  if (signedInAs === null) forgetOtherUsers(userId);
  signedInAs = userId;
  setRecoveryUser(userId);
}
