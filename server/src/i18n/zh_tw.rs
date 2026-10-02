//! Traditional Chinese texts of the server: one entry per text of texts.rs (the English source and what each one is),
//! with the same parameters. Words as in web/src/lib/i18n/glossary.md.

use super::texts::Text::{self, *};

pub(super) const TEXTS: &[(Text, &str)] = &[
    // Every notification email
    (MailOpen, "\n開啟：{link}\n"),
    (MailFooter, "\n—\n你會收到這封郵件，是因為你在 {site} 開啟了電子郵件通知。若不想再收到，請在 {site} 按下鈴鐺並開啟「通知設定」。\n"),
    (MyFiles, "我的檔案"),
    (AllFiles, "全部檔案"),
    (RoleOwner, "擁有者"),
    (RoleManager, "管理者"),
    (RoleEditor, "編輯者"),
    (RoleViewer, "檢視者"),
    // Shared with you
    (SharedSpaceSubject, "{by} 將你加入了空間「{name}」"),
    (SharedItemSubject, "{by} 與你分享了「{name}」"),
    (SharedBody, "{subject}。\n\n你的角色：{role}。\n"),
    (SharedEnds, "你的存取權將於 {ends} 結束。\n"),
    // Access ending
    (ExpiringSubject, "你對「{name}」的存取權即將結束"),
    (ExpiringBody, "你對「{name}」的存取權（{role}）將於 {ends} 結束，之後就無法再開啟。如果還需要，請分享給你的人延長期限。\n"),
    // Space almost full
    (SpaceFullSubject, "空間「{name}」快滿了"),
    (SpaceFullBody, "「{name}」已使用 {used}（共 {quota}，{percent}%）。空間滿了之後就無法再加入檔案。請刪除不再需要的檔案並清空垃圾桶，或請管理員加大空間。\n"),
    // Backups
    (BackupFailingSubject, "備份「{backup}」失敗"),
    (BackupFailingBody, "「{backup}」最新的快照因錯誤而停止：{error}\n"),
    (BackupWaitingSubject, "備份「{backup}」無法連線到存放位置"),
    (BackupWaitingBody, "無法連線到「{backup}」的存放位置：{error}\n每隔幾分鐘會自動再試一次。\n"),
    (BackupOverdueSubject, "備份「{backup}」逾期了"),
    (BackupOverdueBody, "「{backup}」超過預定的時間都沒有完成快照。\n"),
    (BackupOkSubject, "備份「{backup}」恢復正常"),
    (BackupOkBody, "「{backup}」又完成了快照。\n"),
    (BackupNewest, "\n最新的完整快照：{since}。\n"),
    (BackupSee, "\n請到「控制台 › 備份」查看。\n"),
    // Replicas
    (ReplicaDegradedSubject, "複本「{policy}」沒有全部保持"),
    (ReplicaDegradedBody, "「{policy}」應保持 {wanted} 份複本，目前只有 {current} 份是最新的：有目標無法連線、失敗、有損毀的複本或落後。\n"),
    (ReplicaOkSubject, "複本「{policy}」恢復正常"),
    (ReplicaOkBody, "「{policy}」又保持了它的複本。\n"),
    (ReplicaSee, "\n請到「控制台 › 複本」查看。\n"),
    // Files received through a link
    (LinkUploadSubject, "有人透過連結把「{file}」傳到了「{name}」"),
    (LinkUploadBody, "有人透過你建立的收件連結，把「{file}」上傳到「{name}」。\n"),
    // New app password
    (AppPasswordSubject, "你的帳號建立了應用程式密碼「{name}」"),
    (
        AppPasswordBody,
        "你的帳號剛建立了應用程式密碼「{name}」（{access}），來源位址 {ip}。\n\n如果不是你建立的，請在帳號選單的「應用程式密碼」中移除它，並變更你的密碼。\n",
    ),
    (AppPasswordReadOnly, "只能讀取檔案"),
    (AppPasswordReadWrite, "可讀取及變更檔案"),
    // New sign-in method
    (SignInMethodSubject, "你的帳號連結了 {provider} 帳號"),
    (
        SignInMethodBody,
        "你的帳號剛連結了 {provider} 帳號{account}，來源位址 {ip}。之後可以用它登入你的帳號，不需要密碼。\n\n如果不是你連結的，請在帳號選單的「登入方式」中取消連結，並變更你的密碼。\n",
    ),
    (SignInMethodAccount, "（{account}）"),
    // Test email
    (TestSubject, "{site} 的測試郵件"),
    (TestBody, "這是 {site} 寄出的測試郵件。收到這封郵件，表示電子郵件通知可以正常寄送。\n"),
    // Forgot password
    (ResetSubject, "重設 {site} 的密碼"),
    (
        ResetBody,
        "有人要求重設 {site} 帳號「{username}」的密碼。\n\n在一小時內開啟這個連結，設定新密碼（只能使用一次）：\n{link}\n\n如果不是你要求的，請忽略這封郵件，你的密碼不會改變。\n",
    ),
    // The email address changed
    (EmailChangedSubject, "你在 {site} 的電子郵件地址已變更"),
    (
        EmailChangedBody,
        "{site} 帳號「{username}」的電子郵件地址已改為 {new}，之後的通知與重設密碼連結都會寄到那裡。\n\n如果不是你變更的，請立即變更密碼，或聯絡管理員。\n",
    ),
    (EmailChangedNone, "（空白）"),
];
