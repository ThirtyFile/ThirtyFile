//! Simplified Chinese texts of the server: one entry per text of texts.rs (the English source and what each one is),
//! with the same parameters, written like zh_tw.rs. Words as in web/src/lib/i18n/glossary.md.

use super::texts::Text::{self, *};

pub(super) const TEXTS: &[(Text, &str)] = &[
    // Every notification email
    (MailOpen, "\n打开：{link}\n"),
    (MailFooter, "\n—\n你收到这封邮件，是因为你在 {site} 开启了邮件通知。如果不想再收到，请在 {site} 点击铃铛并打开“通知设置”。\n"),
    (MyFiles, "我的文件"),
    (AllFiles, "全部文件"),
    (RoleOwner, "所有者"),
    (RoleManager, "管理者"),
    (RoleEditor, "编辑者"),
    (RoleViewer, "查看者"),
    // Shared with you
    (SharedSpaceSubject, "{by} 将你添加到了空间“{name}”"),
    (SharedItemSubject, "{by} 与你共享了“{name}”"),
    (SharedBody, "{subject}。\n\n你的角色：{role}。\n"),
    (SharedEnds, "你的访问权限将于 {ends} 结束。\n"),
    // Access ending
    (ExpiringSubject, "你对“{name}”的访问权限即将结束"),
    (ExpiringBody, "你对“{name}”的访问权限（{role}）将于 {ends} 结束，之后将无法再打开。如果仍然需要，请让共享给你的人延长期限。\n"),
    // Space almost full
    (SpaceFullSubject, "空间“{name}”快满了"),
    (SpaceFullBody, "“{name}”已使用 {used}（共 {quota}，{percent}%）。空间满了以后将无法再添加文件。请删除不再需要的文件并清空回收站，或请管理员增加空间容量。\n"),
    // Backups
    (BackupFailingSubject, "备份“{backup}”失败"),
    (BackupFailingBody, "“{backup}”最新的快照因错误而中止：{error}\n"),
    (BackupWaitingSubject, "备份“{backup}”无法连接到存储位置"),
    (BackupWaitingBody, "无法连接到“{backup}”的存储位置：{error}\n系统每隔几分钟会自动重试。\n"),
    (BackupOverdueSubject, "备份“{backup}”已逾期"),
    (BackupOverdueBody, "“{backup}”超过预定时间仍未完成快照。\n"),
    (BackupOkSubject, "备份“{backup}”已恢复正常"),
    (BackupOkBody, "“{backup}”又完成了一次完整快照。\n"),
    (BackupNewest, "\n最新的完整快照：{since}。\n"),
    (BackupSee, "\n请在“控制面板 › 备份”中查看。\n"),
    // Replicas
    (ReplicaDegradedSubject, "镜像“{policy}”未全部保持"),
    (ReplicaDegradedBody, "“{policy}”应保持 {wanted} 个镜像，目前只有 {current} 个：有目标位置无法连接、出现故障、存有损坏的镜像或进度落后。\n"),
    (ReplicaOkSubject, "镜像“{policy}”已恢复正常"),
    (ReplicaOkBody, "“{policy}”又保持了全部镜像。\n"),
    (ReplicaSee, "\n请在“控制面板 › 镜像”中查看。\n"),
    // Files received through a link
    (LinkUploadSubject, "有人通过链接将“{file}”上传到了“{name}”"),
    (LinkUploadBody, "有人通过你创建的可接收文件的链接，将“{file}”上传到了“{name}”。\n"),
    // New app password
    (AppPasswordSubject, "你的账号创建了应用密码“{name}”"),
    (AppPasswordBody, "你的账号刚刚创建了应用密码“{name}”（{access}），来源地址 {ip}。\n\n如果不是你创建的，请在账号菜单的“应用密码”中移除它，并修改你的密码。\n"),
    (AppPasswordReadOnly, "只能读取文件"),
    (AppPasswordReadWrite, "读取并更改文件"),
    // New sign-in method
    (SignInMethodSubject, "你的账号关联了 {provider} 账号"),
    (
        SignInMethodBody,
        "你的账号刚刚关联了 {provider} 账号{account}，来源地址 {ip}。今后可以用它登录你的账号，无需密码。\n\n如果不是你关联的，请在账号菜单的“登录方式”中取消关联，并修改你的密码。\n",
    ),
    (SignInMethodAccount, "（{account}）"),
    // Test email
    (TestSubject, "{site} 的测试邮件"),
    (TestBody, "这是 {site} 发送的测试邮件。能收到这封邮件，说明邮件通知可以正常发送。\n"),
    // Forgot password
    (ResetSubject, "重置 {site} 的密码"),
    (
        ResetBody,
        "有人请求重置 {site} 上账号“{username}”的密码。\n\n请在一小时内打开以下链接设置新密码（仅可使用一次）：\n{link}\n\n如果不是你本人操作，请忽略这封邮件，你的密码不会改变。\n",
    ),
    // The email address changed
    (EmailChangedSubject, "你在 {site} 的邮箱地址已更改"),
    (
        EmailChangedBody,
        "{site} 上账号“{username}”的邮箱地址已改为 {new}，今后的通知和重置密码链接都会发送到该地址。\n\n如果不是你本人更改的，请立即修改密码或联系管理员。\n",
    ),
    (EmailChangedNone, "（无）"),
    // Names given to what ThirtyFile creates
    (FilesOf, "{username} 的文件"),
    (ConflictCopy, "{name}（冲突副本）"),
    (RestoredFolder, "还原 {space} {date}"),
];
