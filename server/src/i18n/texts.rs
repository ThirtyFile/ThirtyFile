//! The texts the server writes itself, rather than the page: the emails it sends. English is the source, written here
//! with each text's key ([`Text`]); every other language has its translations in a file of its own (zh_tw.rs,
//! zh_cn.rs, ja.rs), and a text missing there is sent in English.
//!
//! - Parameters are written `{name}`; a translation has exactly the parameters of its English text
//! - The words follow the interface's (web/src/lib/i18n/glossary.md), so an email says what the page says
//! - A language must have every text once it is in [`COMPLETE`] or offered to people (`offered` in mod.rs); a test
//!   checks this

use super::{Lang, ja, zh_cn, zh_tw};

/// Languages whose texts must be complete: a new text needs their translation in the same change. A language that is
/// offered to people must be complete too
#[cfg(test)]
pub const COMPLETE: &[Lang] = &[Lang::En, Lang::ZhTw, Lang::ZhCn, Lang::Ja];

macro_rules! texts {
    ($($(#[doc = $doc:literal])* $name:ident = $en:literal,)*) => {
        /// A text the server writes; see [`english`](Text::english) for each one's English
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Text {
            $($(#[doc = $doc])* $name,)*
        }

        impl Text {
            /// Every text
            #[cfg(test)]
            pub const ALL: &[Text] = &[$(Text::$name,)*];

            /// The English source text
            pub fn english(self) -> &'static str {
                match self {
                    $(Text::$name => $en,)*
                }
            }
        }
    };
}

texts! {
    // ───── Every notification email ─────
    /// After the text, when the email is about something that can be opened
    MailOpen = "\nOpen: {link}\n",
    /// The end of every notification email
    MailFooter = "\n—\nYou get this email because email notifications are on in {site}. To stop them, click the bell in {site} and open Notification settings.\n",
    /// The names of the spaces ThirtyFile names itself, as the page shows them
    MyFiles = "My files",
    AllFiles = "All files",
    /// Roles of people who were given access
    RoleOwner = "Owner",
    RoleManager = "Manager",
    RoleEditor = "Editor",
    RoleViewer = "Viewer",

    // ───── Shared with you ─────
    SharedSpaceSubject = "{by} added you to the space “{name}”",
    SharedItemSubject = "{by} shared “{name}” with you",
    /// `subject`: one of the two above
    SharedBody = "{subject}.\n\nYour role: {role}.\n",
    SharedEnds = "Your access ends on {ends}.\n",

    // ───── Access ending ─────
    ExpiringSubject = "Your access to “{name}” ends soon",
    ExpiringBody = "Your access to “{name}” ({role}) ends on {ends}. After that you can't open it any more. If you still need it, ask the person who shared it with you to extend it.\n",

    // ───── Space almost full ─────
    SpaceFullSubject = "The space “{name}” is almost full",
    SpaceFullBody = "“{name}” uses {used} of {quota} ({percent}%). Once it is full, no more files can be added. Delete files you no longer need and empty the trash, or ask an administrator for more space.\n",

    // ───── Backups (administrators) ─────
    BackupFailingSubject = "The backup “{backup}” failed",
    BackupFailingBody = "The latest snapshot of “{backup}” stopped by an error: {error}\n",
    BackupWaitingSubject = "The backup “{backup}” can't reach its location",
    BackupWaitingBody = "The location of “{backup}” can't be reached: {error}\nIt is tried again every few minutes.\n",
    BackupOverdueSubject = "The backup “{backup}” is overdue",
    BackupOverdueBody = "“{backup}” made no complete snapshot for longer than it should.\n",
    BackupOkSubject = "The backup “{backup}” works again",
    BackupOkBody = "“{backup}” made a complete snapshot again.\n",
    BackupNewest = "\nNewest complete snapshot: {since}.\n",
    BackupSee = "\nSee Control panel › Backups.\n",

    // ───── Replicas (administrators) ─────
    ReplicaDegradedSubject = "The replicas “{policy}” aren't all kept",
    ReplicaDegradedBody = "“{policy}” keeps {current} of the {wanted} copies it should: a target can't be reached, failed, holds damaged copies or is behind.\n",
    ReplicaOkSubject = "The replicas “{policy}” are kept again",
    ReplicaOkBody = "“{policy}” keeps its copies again.\n",
    ReplicaSee = "\nSee Control panel › Replicas.\n",

    // ───── Files received through a link ─────
    LinkUploadSubject = "“{file}” arrived in “{name}” through a link",
    LinkUploadBody = "Someone uploaded “{file}” to “{name}” through a link you made that accepts files.\n",

    // ───── New app password ─────
    AppPasswordSubject = "An app password “{name}” was created for your account",
    /// `access`: one of the two below
    AppPasswordBody = "The app password “{name}” ({access}) was just created for your account, from {ip}.\n\nIf you didn't create it, remove it under App passwords in the account menu and change your password.\n",
    AppPasswordReadOnly = "read files only",
    AppPasswordReadWrite = "read and change files",

    // ───── New sign-in method ─────
    SignInMethodSubject = "A {provider} account was linked to your account",
    /// `account`: [`SignInMethodAccount`](Text::SignInMethodAccount), or nothing when the provider gave no account name
    SignInMethodBody = "A {provider} account{account} was just linked to your account, from {ip}. It can now sign in to your account without the password.\n\nIf you didn't link it, unlink it under Sign-in methods in the account menu and change your password.\n",
    SignInMethodAccount = " ({account})",

    // ───── Control panel › Email › Send test email ─────
    TestSubject = "Test email from {site}",
    TestBody = "This is a test email from {site}. If you can read it, email notifications work.\n",

    // ───── Forgot password ─────
    ResetSubject = "Reset your password for {site}",
    ResetBody = "Someone asked to reset the password of the account \"{username}\" on {site}.\n\nOpen this link within an hour to choose a new password (it works once):\n{link}\n\nIf it wasn't you, ignore this email: your password stays as it is.\n",

    // ───── The email address of an account changed (to the old address) ─────
    EmailChangedSubject = "Your email address on {site} was changed",
    EmailChangedBody = "The email address of the account \"{username}\" on {site} was changed to {new}. Notifications and password reset links go there from now on.\n\nIf you didn't change it, change your password now or contact your administrator.\n",
    /// `new` when the address was removed
    EmailChangedNone = "(none)",

    // ───── Names ThirtyFile gives what it creates in people's folders, in the system default language ─────
    /// The folder a removed personal space's files are moved into
    FilesOf = "Files of {username}",
    /// The copy of a file saved while it was changed in its folder on the server; `name` without the extension, which
    /// follows the text
    ConflictCopy = "{name} (conflict copy)",
    /// The folder a restore from a backup goes into, when the page names none; `date` as 2026-10-01 14.05
    RestoredFolder = "Restored {space} {date}",
}

/// A language's translations
fn translations(lang: Lang) -> &'static [(Text, &'static str)] {
    match lang {
        Lang::En => &[],
        Lang::ZhTw => zh_tw::TEXTS,
        Lang::ZhCn => zh_cn::TEXTS,
        Lang::Ja => ja::TEXTS,
    }
}

/// A text in a language, English when it isn't translated
pub fn text(lang: Lang, t: Text) -> &'static str {
    translations(lang).iter().find(|(k, _)| *k == t).map_or(t.english(), |(_, v)| v)
}

/// A text in a language with its parameters filled in: `{name}` is replaced by the value of `name`. Values are put in
/// as they are, also when they hold braces
pub fn tr(lang: Lang, t: Text, vars: &[(&str, &str)]) -> String {
    fill(text(lang, t), vars)
}

fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}').map(|close| (&after[..close], close)).and_then(|(name, close)| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v, close))) {
            Some((value, close)) => {
                out.push_str(value);
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The parameters of a text: the names in braces
#[cfg(test)]
fn params(s: &str) -> std::collections::BTreeSet<&str> {
    s.split('{')
        .skip(1)
        .filter_map(|p| p.split_once('}').map(|(name, _)| name))
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_are_filled_once_and_unknown_ones_stay() {
        assert_eq!(fill("Hello {who}, {who}!", &[("who", "Amy")]), "Hello Amy, Amy!");
        assert_eq!(fill("{a}{b}", &[("a", "{b}"), ("b", "x")]), "{b}x", "a value isn't filled again");
        assert_eq!(fill("{missing} and {", &[]), "{missing} and {");
        assert_eq!(tr(Lang::En, Text::TestSubject, &[("site", "Drive")]), "Test email from Drive");
    }

    #[test]
    fn a_text_without_a_translation_is_sent_in_english() {
        for lang in Lang::ALL {
            for t in Text::ALL {
                match translations(lang).iter().find(|(k, _)| k == t) {
                    Some((_, s)) => assert_eq!(text(lang, *t), *s),
                    None => assert_eq!(text(lang, *t), t.english(), "{}: {t:?}", lang.code()),
                }
            }
        }
    }

    #[test]
    fn every_language_that_must_be_complete_has_every_text() {
        let mut problems = Vec::new();
        for lang in Lang::ALL {
            let table = translations(lang);
            let required = COMPLETE.contains(&lang) || lang.offered();
            let mut missing = 0;
            for t in Text::ALL {
                match table.iter().filter(|(k, _)| k == t).count() {
                    0 if lang != Lang::En => missing += 1,
                    0 | 1 => {}
                    _ => problems.push(format!("{}: {t:?} is translated more than once", lang.code())),
                }
            }
            for (t, s) in table {
                if params(s) != params(t.english()) {
                    problems.push(format!("{}: {t:?} has the parameters {:?}, the English text {:?}", lang.code(), params(s), params(t.english())));
                }
            }
            if missing > 0 {
                if required {
                    problems.push(format!(
                        "{}: {missing} of the server's texts aren't translated (server/src/i18n/{}.rs)",
                        lang.code(),
                        lang.code().to_lowercase().replace('-', "_")
                    ));
                } else {
                    // Not required yet: the English text is sent meanwhile
                    eprintln!("{}: {missing} of {} server texts not translated yet", lang.code(), Text::ALL.len());
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}
