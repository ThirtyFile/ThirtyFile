//! Directory sign-in settings: where the directory is, who may search it, how people and their groups are found.

use rustls::pki_types::{CertificateDer, pem::PemObject};
use serde::{Deserialize, Serialize};

use crate::{
    error::{AppError, AppResult},
    ldap::{self, filter},
};

pub const SETTING: &str = "directory";
/// What the service account's password is sealed with (`secrets::seal`)
pub const SEAL: &str = "settings:directory";

/// How a person's groups are found
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupMode {
    /// Not at all: no groups are followed, and none can be required
    #[default]
    None,
    /// From the person's own `memberOf` values (Active Directory, FreeIPA, OpenLDAP with the memberof overlay)
    MemberOf,
    /// By searching the groups that have the person as a member (`group_filter`, with `{dn}`)
    Search,
}

/// A group of the directory (its DN) and the group here whose members follow it
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Mapping {
    pub dn: String,
    pub group_id: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// `ldaps://host[:port]`, or `ldap://host[:port]`
    pub url: String,
    /// With `ldap://`: StartTLS before anything is sent. Without it, only to a directory on the private network.
    pub starttls: bool,
    /// The certificate of the authority that made the directory's (PEM), when it is a private one
    pub ca_pem: String,
    /// The account that searches
    pub bind_dn: String,
    /// Its password, sealed (never sent to a page)
    pub bind_password: String,
    pub user_base: String,
    /// Finds a person by what they typed: has `{username}`
    pub user_filter: String,
    /// The stable id of a person (`objectGUID`, `entryUUID`): what an account is linked by
    pub attr_id: String,
    pub attr_username: String,
    pub attr_name: String,
    pub attr_email: String,
    /// Only the members of this group (its DN) may sign in; blank: everyone found
    pub require_group: String,
    pub group_mode: GroupMode,
    pub group_base: String,
    /// With `GroupMode::Search`: the groups that have the person as a member; has `{dn}`
    pub group_filter: String,
    pub mappings: Vec<Mapping>,
    /// A first sign-in creates the account (otherwise only accounts that exist are linked)
    pub create_accounts: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            enabled: false,
            url: String::new(),
            starttls: true,
            ca_pem: String::new(),
            bind_dn: String::new(),
            bind_password: String::new(),
            user_base: String::new(),
            user_filter: String::new(),
            attr_id: "entryUUID".into(),
            attr_username: "uid".into(),
            attr_name: "cn".into(),
            attr_email: "mail".into(),
            require_group: String::new(),
            group_mode: GroupMode::None,
            group_base: String::new(),
            group_filter: String::new(),
            mappings: Vec::new(),
            create_accounts: true,
        }
    }
}

/// Where the directory is, as `url` says it
pub struct Target {
    pub host: String,
    pub port: u16,
    pub security: ldap::Security,
}

impl Settings {
    pub fn target(&self) -> Result<Target, String> {
        let url = reqwest::Url::parse(self.url.trim()).map_err(|_| "The address isn't one: write ldaps://host or ldap://host".to_string())?;
        let security = match url.scheme() {
            "ldaps" => ldap::Security::Ldaps,
            "ldap" if self.starttls => ldap::Security::StartTls,
            "ldap" => ldap::Security::None,
            _ => return Err("The address starts with ldaps:// or ldap://".into()),
        };
        if !url.username().is_empty() || url.password().is_some() || !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
            return Err("The address is only a host and, if it isn't the usual one, a port".into());
        }
        let host = url.host_str().filter(|h| !h.is_empty()).ok_or("The address has no host")?.trim_matches(['[', ']']).to_string();
        let port = url.port().unwrap_or(if security == ldap::Security::Ldaps { 636 } else { 389 });
        Ok(Target { host, port, security })
    }

    /// The certificates of the authority, as DER
    pub fn roots(&self) -> Result<Vec<Vec<u8>>, String> {
        if self.ca_pem.trim().is_empty() {
            return Ok(Vec::new());
        }
        let certs = CertificateDer::pem_slice_iter(self.ca_pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "The certificate isn't PEM: it begins with -----BEGIN CERTIFICATE-----".to_string())?;
        if certs.is_empty() {
            return Err("The certificate isn't PEM: it begins with -----BEGIN CERTIFICATE-----".into());
        }
        Ok(certs.into_iter().map(|c| c.to_vec()).collect())
    }

    /// The connection to make
    pub fn connection(&self) -> Result<ldap::Config, String> {
        let t = self.target()?;
        Ok(ldap::Config { host: t.host, port: t.port, security: t.security, roots: self.roots()?, timeout: std::time::Duration::from_secs(10) })
    }

    /// The service account's password, unsealed
    pub fn password(&self) -> Result<String, String> {
        crate::secrets::open(SEAL, &self.bind_password)
            .map_err(|_| "The service account's password can't be read with this server's key: enter it again".to_string())
    }

    /// The group (lower case) a DN stands for, to compare DNs without regard to case or spaces after commas
    pub fn normalize_dn(dn: &str) -> String {
        dn.split(',').map(|p| p.trim().to_ascii_lowercase()).collect::<Vec<_>>().join(",")
    }

    /// What is wrong with these settings before they are saved or used: the first problem, in words
    pub fn check(&self) -> Result<(), String> {
        self.target()?;
        self.roots()?;
        let need = |text: &str, what: &str| if text.trim().is_empty() { Err(format!("Enter {what}")) } else { Ok(()) };
        need(&self.bind_dn, "the service account's name (its DN)")?;
        need(&self.user_base, "where to search for people (the search base)")?;
        need(&self.user_filter, "the filter that finds a person")?;
        filter::for_user(&self.user_filter, "test").map_err(|e| format!("The filter that finds a person: {}", e.0))?;
        need(&self.attr_id, "the attribute with a person's stable id")?;
        need(&self.attr_username, "the attribute with the user name")?;
        match self.group_mode {
            GroupMode::None => {
                if !self.require_group.trim().is_empty() || !self.mappings.is_empty() {
                    return Err("Choose how groups are found, or take away the group that is required and the groups that are followed".into());
                }
            }
            GroupMode::MemberOf => {}
            GroupMode::Search => {
                need(&self.group_base, "where to search for groups")?;
                if !self.group_filter.contains("{dn}") {
                    return Err("The filter that finds a person's groups has to contain {dn}".into());
                }
                filter::parse(&self.group_filter.replace("{dn}", "cn=test")).map_err(|e| format!("The filter that finds a person's groups: {}", e.0))?;
            }
        }
        Ok(())
    }
}

pub async fn load(db: &sqlx::SqlitePool) -> Settings {
    match crate::db::get_setting(db, SETTING).await {
        Ok(Some(v)) => serde_json::from_str(&v).unwrap_or_default(),
        _ => Settings::default(),
    }
}

pub async fn store(conn: &mut sqlx::SqliteConnection, settings: &Settings) -> AppResult<()> {
    crate::db::set_json_setting(conn, SETTING, settings).await
}

/// A directory with `ldap://` and no StartTLS is only for the private network, as plain FTP is
pub async fn check_plain_target(settings: &Settings) -> AppResult<()> {
    let Ok(target) = settings.target() else { return Ok(()) };
    if target.security != ldap::Security::None {
        return Ok(());
    }
    let addrs: Vec<std::net::IpAddr> = match target.host.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => match tokio::time::timeout(std::time::Duration::from_secs(5), tokio::net::lookup_host((target.host.as_str(), target.port))).await {
            Ok(Ok(list)) => list.map(|a| a.ip()).collect(),
            _ => {
                return Err(AppError::bad_request(format!("An unencrypted connection is only allowed to private network addresses: can't resolve {}", target.host)));
            }
        },
    };
    match addrs.into_iter().find(|ip| !crate::storage::is_private_ip(ip)) {
        Some(ip) => Err(AppError::bad_request(format!(
            "An unencrypted connection is only allowed to private network addresses: {} is a public address ({ip}). Use ldaps:// or StartTLS.",
            target.host
        ))),
        None => Ok(()),
    }
}
