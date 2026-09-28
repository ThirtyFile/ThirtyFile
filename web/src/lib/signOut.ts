import { clearDownloads } from "@/downloads";
import { cancelAll } from "@/uploads";

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
    // The folders expanded in the navigation pane (components/FolderTree.tsx)
    localStorage.removeItem("tf-tree-expanded");
    sessionStorage.removeItem(`tf-tabs-${userId}`);
  } catch {
    // Storage blocked by the browser: nothing was kept there either
  }
  window.location.assign("/login");
}

let signedInAs: number | null = null;

/**
 * Called with the signed-in user whenever it is known. After a session expired, someone else may sign in in the same
 * tab: then the page is loaded afresh, so the previous user's drafts and lists in memory don't reach them.
 */
export function noteSignedIn(userId: number) {
  if (signedInAs !== null && signedInAs !== userId) {
    window.location.reload();
    return;
  }
  signedInAs = userId;
}
