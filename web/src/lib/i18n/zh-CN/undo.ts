/** Simplified Chinese translations: large uploads, confirmations and undo */
export default {
  "{n} waiting": "{n} 个等待中",
  "{n} complete": "{n} 个已完成",
  "{n} more not shown": "另有 {n} 个未显示",
  "Retry all failed": "全部重试",
  // Confirmations
  "Leave “{name}”?": "要退出“{name}”的共享吗？",
  "You'll lose your access to it right away. Only someone who manages it can give it back.": "你会立即失去访问权限，只有管理者能再次授予。",
  "Remove access for {name}?": "要移除 {name} 的访问权限吗？",
  "They'll lose their access to it right away. You can give it back later.": "对方会立即失去访问权限，之后仍可再次授予。",
  "Cancel downloads": "取消下载",
  "Cancel uploads": "取消上传",
  "Unlink your {provider} account?": "要取消关联你的 {provider} 账号吗？",
  "This file has unsaved changes. If you leave it, your changes will be lost.": "文件有未保存的更改，离开后更改将会丢失。",
  "Leave without saving": "不保存并离开",
  "Delete this share link?": "要删除这个分享链接吗？",
  "People who have the link can no longer open it. A new link would have a different address.": "持有链接的人将无法再打开。新建的链接会是不同的地址。",
  "Disable account \"{name}\"?": "要停用账号“{name}”吗？",
  "They can no longer sign in, and their share links stop working until the account is enabled again.": "在重新启用账号之前，对方无法登录，其分享链接也会失效。",
  "Remove the logo?": "要移除 Logo 吗？",
  "Remove the dark mode logo?": "要移除深色模式 Logo 吗？",
  "Remove the background image?": "要移除背景图片吗？",
  "The image is deleted. To use it again, you'll need to upload it again.": "图片会被删除，如需再次使用须重新上传。",
  "{n} tab has unsaved changes. If you close it, the changes will be lost.|{n} tabs have unsaved changes. If you close them, the changes will be lost.": "{n} 个标签页有未保存的更改，关闭后更改将会丢失。",
  // Undo
  "Renamed to \"{name}\"": "已重命名为“{name}”",
  "Renamed back": "已恢复原名称",
  "Moved back": "已移回原位置",
  // The menu item that takes the last action back (components/explorer/menus.tsx)
  "Undo delete": "撤销删除",
  "Undo rename": "撤销重命名",
  "Undo move": "撤销移动",
} satisfies Record<string, string>;
