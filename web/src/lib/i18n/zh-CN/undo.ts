/** Simplified Chinese translations: large uploads, confirmations and undo */
export default {
  "{n} waiting": "{n} 個等待中",
  "{n} complete": "{n} 個已完成",
  "{n} more not shown": "另有 {n} 個未顯示",
  "Retry all failed": "全部重試",
  // Confirmations
  "Leave “{name}”?": "要退出「{name}」的共用嗎？",
  "You'll lose your access to it right away. Only someone who manages it can give it back.": "你會立即失去存取權限，只有管理者能再次授予。",
  "Remove access for {name}?": "要移除 {name} 的存取權限嗎？",
  "They'll lose their access to it right away. You can give it back later.": "對方會立即失去存取權限，之後仍可再次授予。",
  "Cancel downloads": "取消下載",
  "Cancel uploads": "取消上傳",
  "Unlink your {provider} account?": "要取消連結你的 {provider} 帳號嗎？",
  "This file has unsaved changes. If you leave it, your changes will be lost.": "檔案有未儲存的變更，離開後變更將會遺失。",
  "Leave without saving": "不儲存並離開",
  "Delete this share link?": "要刪除這個分享連結嗎？",
  "People who have the link can no longer open it. A new link would have a different address.": "持有連結的人將無法再開啟。新建立的連結會是不同的網址。",
  "Disable account \"{name}\"?": "要停用帳號「{name}」嗎？",
  "They can no longer sign in, and their share links stop working until the account is enabled again.": "在重新啟用帳號之前，對方無法登入，其分享連結也會失效。",
  "Remove the logo?": "要移除標誌嗎？",
  "Remove the dark mode logo?": "要移除深色模式標誌嗎？",
  "Remove the background image?": "要移除背景圖片嗎？",
  "The image is deleted. To use it again, you'll need to upload it again.": "圖片會被刪除，若要再使用須重新上傳。",
  "{n} tab has unsaved changes. If you close it, the changes will be lost.|{n} tabs have unsaved changes. If you close them, the changes will be lost.": "{n} 個分頁有未儲存的變更，關閉後變更將會遺失。",
  // Undo
  "Renamed to \"{name}\"": "已重新命名為「{name}」",
  "Renamed back": "已改回原名稱",
  "Moved back": "已移回原位置",
  // The menu item that takes the last action back (components/explorer/menus.tsx)
  "Undo delete": "復原刪除",
  "Undo rename": "復原重新命名",
  "Undo move": "復原移動",
} satisfies Record<string, string>;
