//! What is wrong right now, looked at once a minute: a storage location that is offline or fails, a disk that is
//! nearly full, a burst of failed sign-ins, an FTPS certificate about to end.
//!
//! Each is a `Condition` with a key that stays the same for as long as it is the same problem (`disk:nas`, not the
//! percent), so the queue (queue.rs) can tell a new one from one that goes on or is over. What they hold is names of
//! locations, addresses, accounts and numbers: never an item's name.

use serde_json::{Value, json};

use super::Config;
use crate::{error::AppResult, state::AppState};

/// How long before a certificate ends it is told
pub const CERTIFICATE_DAYS: i64 = 14;

#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub key: String,
    pub kind: &'static str,
    pub params: Value,
}

/// Everything wrong now that the settings ask to be told about
pub async fn current(st: &AppState, c: &Config, t: i64) -> AppResult<Vec<Condition>> {
    let mut out = vec![];
    if c.usage || c.offline || c.data_disk {
        out.extend(from_usage(st, c, t).await?);
    }
    if c.sign_ins {
        out.extend(sign_ins(st, c, t).await?);
    }
    if c.certificate {
        out.extend(certificate(st, t));
    }
    Ok(out)
}

/// The alerts of the usage page (Control panel › Storage usage), by what they are about
async fn from_usage(st: &AppState, c: &Config, t: i64) -> AppResult<Vec<Condition>> {
    let overview = crate::usage::api::build_overview(st, t).await?;
    let name = |id: &str| overview.locations.iter().find(|l| l.id == id).map_or_else(|| id.to_string(), |l| l.name.clone());
    let mut out = vec![];
    for a in &overview.alerts {
        let percent = a.value.round() as i64;
        let (kind, wanted, params) = match (a.kind, a.location.as_str()) {
            ("disk", "") => ("data_disk", c.data_disk, json!({ "percent": percent })),
            ("disk", id) => ("usage_disk", c.usage, json!({ "location": name(id), "percent": percent })),
            ("errors", id) => ("usage_errors", c.usage, json!({ "location": name(id), "percent": percent })),
            ("offline", id) => ("offline", c.offline, json!({ "location": name(id) })),
            _ => continue,
        };
        if wanted {
            out.push(Condition { key: format!("{kind}:{}", a.location), kind, params });
        }
    }
    Ok(out)
}

/// Failed sign-ins from one address, or for one account, passing the limit within the last minutes. Only accounts that
/// exist are named: a name typed that isn't one may be a password typed in the wrong box.
async fn sign_ins(st: &AppState, c: &Config, t: i64) -> AppResult<Vec<Condition>> {
    let since = t - i64::from(c.sign_in_minutes) * 60;
    let limit = i64::from(c.sign_in_failures);
    let by_address: Vec<(String, i64)> =
        sqlx::query_as("SELECT ip, COUNT(*) FROM login_log WHERE event = 'failed' AND at > ?1 AND ip <> '' GROUP BY ip HAVING COUNT(*) >= ?2 ORDER BY ip")
            .bind(since)
            .bind(limit)
            .fetch_all(&st.db)
            .await?;
    let by_account: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT u.id, u.username, COUNT(*) FROM login_log l JOIN users u ON u.id = l.user_id WHERE l.event = 'failed' AND l.at > ?1 GROUP BY u.id HAVING COUNT(*) >= ?2 ORDER BY u.id",
    )
    .bind(since)
    .bind(limit)
    .fetch_all(&st.db)
    .await?;
    let minutes = c.sign_in_minutes;
    let mut out: Vec<Condition> = by_address
        .into_iter()
        .map(|(ip, count)| Condition { key: format!("sign_ins_ip:{ip}"), kind: "sign_ins_ip", params: json!({ "ip": ip, "count": count, "minutes": minutes }) })
        .collect();
    out.extend(by_account.into_iter().map(|(id, username, count)| Condition {
        key: format!("sign_ins_user:{id}"),
        kind: "sign_ins_user",
        params: json!({ "username": username, "count": count, "minutes": minutes }),
    }));
    Ok(out)
}

/// The FTPS certificate the listener serves, when it ends within `CERTIFICATE_DAYS` (or already has)
fn certificate(st: &AppState, t: i64) -> Option<Condition> {
    let ends = st.part::<crate::ftp::Memory>().status.read().cert_expires_at?;
    let days = (ends - t).div_euclid(86400);
    (days <= CERTIFICATE_DAYS).then(|| Condition { key: "certificate".into(), kind: "certificate", params: json!({ "days": days.max(0), "expires_at": ends }) })
}
