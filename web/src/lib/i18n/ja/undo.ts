/** Japanese translations: large uploads, confirmations and undo */
export default {
  "{n} waiting": "{n} 件待機中",
  "{n} complete": "{n} 件完了",
  "{n} more not shown": "ほか {n} 件は非表示",
  "Retry all failed": "失敗したものをすべて再試行",
  // Confirmations
  "Leave “{name}”?": "「{name}」の共有から外れますか？",
  "You'll lose your access to it right away. Only someone who manages it can give it back.": "すぐにアクセスできなくなります。もう一度アクセスを許可できるのは、管理している人だけです。",
  "Remove access for {name}?": "{name} のアクセス許可を削除しますか？",
  "They'll lose their access to it right away. You can give it back later.": "相手はすぐにアクセスできなくなります。後でもう一度許可することもできます。",
  "Cancel downloads": "ダウンロードをキャンセル",
  "Cancel uploads": "アップロードをキャンセル",
  "Unlink your {provider} account?": "{provider} アカウントのリンクを解除しますか？",
  "This file has unsaved changes. If you leave it, your changes will be lost.": "このファイルには保存されていない変更があります。閉じると変更は失われます。",
  "Leave without saving": "保存せずに閉じる",
  "Delete this share link?": "この共有リンクを削除しますか？",
  "People who have the link can no longer open it. A new link would have a different address.": "リンクを知っている人は開けなくなります。新しいリンクを作成すると、別のアドレスになります。",
  "Disable account \"{name}\"?": "アカウント「{name}」を無効にしますか？",
  "They can no longer sign in, and their share links stop working until the account is enabled again.": "アカウントが再び有効になるまで、このユーザーはサインインできず、共有リンクも機能しなくなります。",
  "Remove the logo?": "ロゴを削除しますか？",
  "Remove the dark mode logo?": "ダークモードのロゴを削除しますか？",
  "Remove the background image?": "背景画像を削除しますか？",
  "The image is deleted. To use it again, you'll need to upload it again.": "画像は削除されます。もう一度使うには、再度アップロードする必要があります。",
  "{n} tab has unsaved changes. If you close it, the changes will be lost.|{n} tabs have unsaved changes. If you close them, the changes will be lost.": "{n} 個のタブに保存されていない変更があります。閉じると変更は失われます。",
  // Undo
  "Renamed to \"{name}\"": "「{name}」に名前を変更しました",
  "Renamed back": "元の名前に戻しました",
  "Moved back": "元の場所に戻しました",
  // The menu item that takes the last action back (components/explorer/menus.tsx)
  "Undo delete": "削除を元に戻す",
  "Undo rename": "名前の変更を元に戻す",
  "Undo move": "移動を元に戻す",
} satisfies Record<string, string>;
