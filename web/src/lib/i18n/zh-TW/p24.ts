/** Traditional Chinese translations (English source text → Traditional Chinese): new spaces as folders on the server (deleting a space or a user keeps the folder) */
export default {
  "They go into a new folder named \"Files of {name}\" at the top of that space, and count toward its size. Their trash stays in their folder on the server.": "檔案會放進該空間最上層一個名為「Files of {name}」的新資料夾，並計入該空間的容量。他的垃圾桶留在伺服器上他的資料夾中。",
  "Remove their files from ThirtyFile": "從 ThirtyFile 移除他的檔案",
  "Their folder on the server, {path}, is kept with the files in it: delete it there when it's no longer needed.": "伺服器上的資料夾 {path} 會連同其中的檔案一起保留，不再需要時請在那裡刪除。",
  "The space is removed from ThirtyFile. Its folder on the server, {path}, is kept with the files in it ({size}): delete it there when it's no longer needed.": "空間會從 ThirtyFile 移除。伺服器上的資料夾 {path} 會連同其中的檔案（{size}）一起保留，不再需要時請在那裡刪除。",
  "The space is removed from ThirtyFile and no member will be able to access it. Its folder on the server is kept with the files in it ({size}), for an administrator to delete.": "空間會從 ThirtyFile 移除，所有成員都無法再存取。伺服器上的資料夾會連同其中的檔案（{size}）一起保留，由管理員決定是否刪除。",
  // App passwords ask who it is again, and are announced
  "Your current password": "目前的密碼",
  "Code from your app": "App 上的驗證碼",
  "For your security, app passwords can only be created within 10 minutes of signing in.": "為了安全，只能在登入後 10 分鐘內建立應用程式密碼。",
  "New app password": "新的應用程式密碼",
  "An app password is created for your account": "你的帳號建立了應用程式密碼",
  "An app password “{name}” was created for your account": "你的帳號建立了應用程式密碼「{name}」",
  "From {ip}. If you didn't create it, remove it under App passwords and change your password.": "來源位址 {ip}。如果不是你建立的，請在「應用程式密碼」中移除它，並變更你的密碼。",
  "Your account isn't allowed to share, so you can't change who has access. Ask an administrator.": "你的帳號沒有分享權限，因此無法變更存取權。請洽管理員。",
  "Show {n} more folders…": "再顯示 {n} 個資料夾…",
  "{n} dropped item couldn't be read and was skipped.|{n} dropped items couldn't be read and were skipped.": "有 {n} 個拖放的項目無法讀取，已略過。",
} satisfies Record<string, string>;
