//! What an alert says: in an email, in each administrator's language, and in a webhook's body in English. Only what
//! the conditions hold goes in: names of locations, addresses, accounts and numbers.

use serde_json::Value;

use crate::i18n::{Lang, Text, tr};

fn text<'a>(params: &'a Value, name: &str) -> &'a str {
    params[name].as_str().unwrap_or_default()
}

fn number(params: &Value, name: &str) -> String {
    params[name].as_i64().unwrap_or_default().to_string()
}

/// What the alert is about, as a sentence without a full stop ("The storage location “NAS” can't be reached")
pub fn what(kind: &str, params: &Value, lang: Lang) -> String {
    let location = text(params, "location");
    let percent = number(params, "percent");
    match kind {
        "usage_disk" => tr(lang, Text::AlertWhatUsageDisk, &[("location", location), ("percent", &percent)]),
        "data_disk" => tr(lang, Text::AlertWhatDataDisk, &[("percent", &percent)]),
        "usage_errors" => tr(lang, Text::AlertWhatErrors, &[("location", location), ("percent", &percent)]),
        "offline" => tr(lang, Text::AlertWhatOffline, &[("location", location)]),
        "directory" => tr(lang, Text::AlertWhatDirectory, &[]),
        "sign_ins_ip" => {
            tr(lang, Text::AlertWhatSignInsIp, &[("count", &number(params, "count")), ("ip", text(params, "ip")), ("minutes", &number(params, "minutes"))])
        }
        "sign_ins_user" => tr(
            lang,
            Text::AlertWhatSignInsUser,
            &[("count", &number(params, "count")), ("username", text(params, "username")), ("minutes", &number(params, "minutes"))],
        ),
        "certificate" => tr(lang, Text::AlertWhatCertificate, &[("days", &number(params, "days"))]),
        "backup" | "replica" => {
            // The database backups' name is in each person's language
            let database = (params["database"] == true).then(|| tr(lang, Text::DatabaseBackup, &[]));
            let name = database.as_deref().unwrap_or_else(|| text(params, "name"));
            tr(lang, if kind == "backup" { Text::AlertWhatBackup } else { Text::AlertWhatReplica }, &[("name", name)])
        }
        _ => kind.to_string(),
    }
}

/// The subject and text of an alert email (`state`: "firing" or "resolved")
pub fn email(state: &str, kind: &str, params: &Value, lang: Lang) -> (String, String) {
    let what = what(kind, params, lang);
    if state == "resolved" {
        return (tr(lang, Text::AlertResolvedSubject, &[("what", &what)]), tr(lang, Text::AlertResolvedBody, &[("what", &what)]));
    }
    let reason = params["error"].as_str().filter(|e| !e.is_empty()).map(|e| tr(lang, Text::AlertReason, &[("error", e)])).unwrap_or_default();
    (tr(lang, Text::AlertFiringSubject, &[("what", &what)]), tr(lang, Text::AlertFiringBody, &[("what", &what), ("reason", &reason)]))
}

/// The test email of Control panel › Administrator alerts
pub fn test_email(lang: Lang, site: &str) -> (String, String) {
    (tr(lang, Text::AlertTestSubject, &[("site", site)]), tr(lang, Text::AlertTestBody, &[("site", site)]))
}
