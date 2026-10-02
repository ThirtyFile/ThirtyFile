//! Japanese texts of the server: one entry per text of texts.rs (the English source and what each one is), with the
//! same parameters, written like zh_tw.rs. Words as in web/src/lib/i18n/glossary.md: です・ます in sentences.

use super::texts::Text::{self, *};

pub(super) const TEXTS: &[(Text, &str)] = &[
    // Every notification email
    (MailOpen, "\n開く：{link}\n"),
    (
        MailFooter,
        "\n—\nこのメールは、{site} でメール通知がオンになっているため送信されています。停止するには、{site} でベルをクリックして「通知の設定」を開いてください。\n",
    ),
    (MyFiles, "マイファイル"),
    (AllFiles, "すべてのファイル"),
    (RoleOwner, "所有者"),
    (RoleManager, "管理者"),
    (RoleEditor, "編集者"),
    (RoleViewer, "閲覧者"),
    // Shared with you
    (SharedSpaceSubject, "{by} があなたをスペース「{name}」に追加しました"),
    (SharedItemSubject, "{by} が「{name}」をあなたと共有しました"),
    (SharedBody, "{subject}。\n\nあなたのロール：{role}。\n"),
    (SharedEnds, "アクセスは {ends} に終了します。\n"),
    // Access ending
    (ExpiringSubject, "「{name}」へのアクセスがまもなく終了します"),
    (
        ExpiringBody,
        "「{name}」へのアクセス（{role}）は {ends} に終了し、その後は開けなくなります。引き続き必要な場合は、共有した人に期限の延長を依頼してください。\n",
    ),
    // Space almost full
    (SpaceFullSubject, "スペース「{name}」がまもなくいっぱいになります"),
    (
        SpaceFullBody,
        "「{name}」は {quota} のうち {used}（{percent}%）を使用しています。いっぱいになると、ファイルを追加できなくなります。不要なファイルを削除してごみ箱を空にするか、管理者に容量の追加を依頼してください。\n",
    ),
    // Backups
    (BackupFailingSubject, "バックアップ「{backup}」が失敗しました"),
    (BackupFailingBody, "「{backup}」の最新のスナップショットがエラーで停止しました：{error}\n"),
    (BackupWaitingSubject, "バックアップ「{backup}」の保存先に接続できません"),
    (BackupWaitingBody, "「{backup}」の保存先に接続できません：{error}\n数分ごとに自動で再試行します。\n"),
    (BackupOverdueSubject, "バックアップ「{backup}」が遅れています"),
    (BackupOverdueBody, "「{backup}」は予定の時間を過ぎても完全なスナップショットを作成していません。\n"),
    (BackupOkSubject, "バックアップ「{backup}」が復旧しました"),
    (BackupOkBody, "「{backup}」が再び完全なスナップショットを作成しました。\n"),
    (BackupNewest, "\n最新の完全なスナップショット：{since}。\n"),
    (BackupSee, "\n「コントロールパネル › バックアップ」を確認してください。\n"),
    // Replicas
    (ReplicaDegradedSubject, "ミラー「{policy}」の一部が保持されていません"),
    (
        ReplicaDegradedBody,
        "「{policy}」は {wanted} 個のミラーを保持する必要がありますが、現在は {current} 個です。ミラー先に接続できない、障害が発生している、破損したミラーがある、または同期が遅れています。\n",
    ),
    (ReplicaOkSubject, "ミラー「{policy}」が復旧しました"),
    (ReplicaOkBody, "「{policy}」は再びすべてのミラーを保持しています。\n"),
    (ReplicaSee, "\n「コントロールパネル › ミラー」を確認してください。\n"),
    // Files received through a link
    (LinkUploadSubject, "リンク経由で「{file}」が「{name}」に届きました"),
    (LinkUploadBody, "あなたが作成したファイルを受け付けるリンクから、「{file}」が「{name}」にアップロードされました。\n"),
    // New app password
    (AppPasswordSubject, "アカウントにアプリパスワード「{name}」が作成されました"),
    (
        AppPasswordBody,
        "あなたのアカウントにアプリパスワード「{name}」（{access}）が作成されました。作成元：{ip}。\n\n心当たりがない場合は、アカウントメニューの「アプリパスワード」で削除し、パスワードを変更してください。\n",
    ),
    (AppPasswordReadOnly, "ファイルの読み取りのみ"),
    (AppPasswordReadWrite, "ファイルの読み取りと変更"),
    // New sign-in method
    (SignInMethodSubject, "アカウントに {provider} アカウントがリンクされました"),
    (
        SignInMethodBody,
        "あなたのアカウントに {provider} アカウント{account}がリンクされました。リンク元：{ip}。今後はパスワードなしでこのアカウントにサインインできます。\n\n心当たりがない場合は、アカウントメニューの「サインイン方法」でリンクを解除し、パスワードを変更してください。\n",
    ),
    (SignInMethodAccount, "（{account}）"),
    // Test email
    (TestSubject, "{site} からのテストメール"),
    (TestBody, "これは {site} からのテストメールです。このメールが届いていれば、メール通知は正常に送信できます。\n"),
    // Forgot password
    (ResetSubject, "{site} のパスワードのリセット"),
    (
        ResetBody,
        "{site} のアカウント「{username}」のパスワードのリセットが要求されました。\n\n1 時間以内に次のリンクを開いて、新しいパスワードを設定してください（使用できるのは 1 回だけです）：\n{link}\n\n心当たりがない場合は、このメールを無視してください。パスワードは変更されません。\n",
    ),
    // The email address changed
    (EmailChangedSubject, "{site} のメールアドレスが変更されました"),
    (
        EmailChangedBody,
        "{site} のアカウント「{username}」のメールアドレスが {new} に変更されました。今後、通知とパスワードのリセット用リンクはこのアドレスに送信されます。\n\n変更した覚えがない場合は、すぐにパスワードを変更するか、管理者に連絡してください。\n",
    ),
    (EmailChangedNone, "（なし）"),
    // Names given to what ThirtyFile creates
    (FilesOf, "{username} のファイル"),
    (ConflictCopy, "{name}（競合コピー）"),
    (RestoredFolder, "復元された {space} {date}"),
];
