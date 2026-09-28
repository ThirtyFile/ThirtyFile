import { clearDownloads } from "@/downloads";
import { cancelAll } from "@/uploads";

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

/** Removes the expanded folders of the tree kept for anyone but `userId` (someone else signed in after them) */
function forgetOtherTrees(userId: number) {
  try {
    const own = treeStorageKey(userId);
    for (const key of Object.keys(localStorage)) {
      if (key.startsWith(TREE_STORAGE_PREFIX) && key !== own) localStorage.removeItem(key);
    }
  } catch {
    // Storage blocked by the browser: nothing was kept there either
  }
}

let signedInAs: number | null = null;

/**
 * Called with the signed-in user whenever it is known. After a session expired, someone else may sign in in the same
 * tab: then the page is loaded afresh, so the previous user's drafts and lists in memory don't reach them. The folders
 * another user expanded in the tree are forgotten, so the next person doesn't see where they have been.
 */
export function noteSignedIn(userId: number) {
  if (signedInAs !== null && signedInAs !== userId) {
    window.location.reload();
    return;
  }
  if (signedInAs === null) forgetOtherTrees(userId);
  signedInAs = userId;
}
