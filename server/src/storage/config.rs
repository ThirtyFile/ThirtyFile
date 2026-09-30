//! A location's settings: checked, tidied and turned into its backend

use super::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct S3Config {
    /// Blank means AWS; enter the endpoint URL for R2, MinIO, etc.
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub bucket: String,
    /// Object key prefix, so several systems can share one bucket
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub access_key_id: String,
    #[serde(default)]
    pub secret_access_key: String,
    /// Path-style URLs (usually needed for MinIO)
    #[serde(default)]
    pub path_style: bool,
    /// Allow http (private networks only)
    #[serde(default)]
    pub allow_http: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocalConfig {
    /// Absolute path of the folder (may be a NAS mount point)
    #[serde(default)]
    pub path: String,
}

/// Tidies up S3 settings before saving:
/// - If the endpoint was pasted as `https://host/bucket`, split out the bucket and keep only the host
/// - The Cloudflare R2 region is always `auto`
/// - For AWS (blank endpoint), always look up the bucket's actual region and correct it, avoiding signature region errors (301 / AuthorizationHeaderMalformed)
pub async fn normalize(kind: &str, config: serde_json::Value) -> serde_json::Value {
    if kind == "sftp" || kind == "ftp" {
        return normalize_host(config);
    }
    if kind != "s3" {
        return config;
    }
    let Ok(mut cfg) = serde_json::from_value::<S3Config>(config.clone()) else { return config };
    let endpoint = cfg.endpoint.trim().trim_end_matches('/').to_string();
    if let Some((scheme, rest)) = endpoint.split_once("://") {
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let path_bucket = path.split('/').next().unwrap_or_default();
        if cfg.bucket.trim().is_empty() && !path_bucket.is_empty() {
            cfg.bucket = path_bucket.to_string();
        }
        cfg.endpoint = format!("{scheme}://{host}");
        if host.to_ascii_lowercase().ends_with(".r2.cloudflarestorage.com") {
            cfg.region = "auto".into();
        }
    }
    if cfg.endpoint.is_empty() && !cfg.bucket.trim().is_empty() {
        let options = object_store::ClientOptions::new();
        let lookup = object_store::aws::resolve_bucket_region(cfg.bucket.trim(), &options);
        if let Ok(Ok(region)) = tokio::time::timeout(std::time::Duration::from_secs(10), lookup).await {
            cfg.region = region;
        }
    }
    serde_json::to_value(cfg).unwrap_or(config)
}

/// Unencrypted connections (S3 over http, FTP without TLS) or ones that skip certificate verification (FTPS) are only allowed to private addresses:
/// the host name is resolved first and rejected if any result is a public IP
pub async fn check_insecure_target(kind: &str, config: &serde_json::Value) -> Result<(), String> {
    let (host, port, reason) = match kind {
        "s3" => {
            let cfg: S3Config = serde_json::from_value(config.clone()).unwrap_or_default();
            let endpoint = cfg.endpoint.trim();
            // A blank endpoint means AWS (https)
            if endpoint.is_empty() {
                return Ok(());
            }
            let url = reqwest::Url::parse(endpoint).map_err(|_| "Invalid endpoint URL".to_string())?;
            if url.scheme() != "http" {
                return Ok(());
            }
            let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']).to_string();
            (host, url.port_or_known_default().unwrap_or(80), "HTTP (unencrypted)")
        }
        "ftp" => {
            let cfg: crate::storage::ftp::FtpConfig = serde_json::from_value(config.clone()).unwrap_or_default();
            let reason = if !cfg.tls {
                "Unencrypted FTP"
            } else if cfg.tls_insecure {
                "Skipping certificate verification"
            } else {
                return Ok(());
            };
            (cfg.host.trim().to_string(), cfg.port(), reason)
        }
        _ => return Ok(()),
    };
    if host.is_empty() {
        return Err(format!("{reason} requires a host address"));
    }
    // Only checked when settings are saved (guards against administrator mistakes; later DNS changes aren't handled)
    let addrs: Vec<std::net::IpAddr> = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => match tokio::time::timeout(std::time::Duration::from_secs(5), tokio::net::lookup_host((host.as_str(), port))).await {
            Ok(Ok(list)) => list.map(|a| a.ip()).collect(),
            // Not allowed when it can't be confirmed to be a private address
            _ => return Err(format!("{reason} is only allowed to private network addresses: can't resolve {host}")),
        },
    };
    match addrs.into_iter().find(|ip| !is_private_ip(ip)) {
        Some(ip) => Err(format!("{reason} is only allowed to private network addresses: {host} is a public address ({ip})")),
        None => Ok(()),
    }
}

/// Private ranges, loopback, link-local, and 100.64.0.0/10 (carrier-grade NAT, also commonly used by VPNs such as Tailscale)
pub fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(&std::net::IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// SFTP / FTP: when the host field is pasted as `sftp://host:2222/data`, split out the port and folder
pub(crate) fn normalize_host(mut config: serde_json::Value) -> serde_json::Value {
    let Some(obj) = config.as_object_mut() else { return config };
    let raw = obj.get("host").and_then(|v| v.as_str()).unwrap_or_default().trim().to_string();
    let rest = raw.split_once("://").map(|(_, r)| r).unwrap_or(&raw);
    let (hostport, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, String::new()));
    // Also split out the username when it precedes the host (user@host)
    let hostport = match hostport.rsplit_once('@') {
        Some((user, h)) => {
            if obj.get("username").and_then(|v| v.as_str()).is_none_or(str::is_empty) {
                obj.insert("username".into(), user.into());
            }
            h
        }
        None => hostport,
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') || h.starts_with('[') => match p.parse::<u16>() {
            Ok(port) => (h.trim_matches(['[', ']']).to_string(), Some(port)),
            Err(_) => (hostport.to_string(), None),
        },
        _ => (hostport.to_string(), None),
    };
    obj.insert("host".into(), host.into());
    if let Some(port) = port {
        obj.insert("port".into(), port.into());
    }
    if !path.is_empty() && obj.get("path").and_then(|v| v.as_str()).is_none_or(|p| p.trim().is_empty()) {
        obj.insert("path".into(), path.trim_end_matches('/').to_string().into());
    }
    config
}

/// The folder of a Local folder location (`id`): the built-in location's is always `default_root`
pub fn local_root(id: &str, config: &serde_json::Value, default_root: &Path) -> io::Result<PathBuf> {
    if id == BUILTIN {
        return Ok(default_root.to_path_buf());
    }
    let cfg: LocalConfig = serde_json::from_value(config.clone()).unwrap_or_default();
    let p = PathBuf::from(cfg.path.trim());
    if !p.is_absolute() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Enter an absolute path"));
    }
    Ok(p)
}

/// Builds the backend of the storage location `id` from its settings. Nothing is created or written: a Local folder
/// location's folder is checked on use (`LocalStorage::verify`).
pub fn build(id: &str, kind: &str, config: &serde_json::Value, default_root: &Path) -> io::Result<Arc<dyn Storage>> {
    match kind {
        "local" => Ok(Arc::new(LocalStorage::new(local_root(id, config, default_root)?, id))),
        "s3" => {
            let cfg: S3Config = serde_json::from_value(config.clone()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
            if cfg.bucket.trim().is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "Enter a bucket name"));
            }
            Ok(Arc::new(S3Storage::new(&cfg)?))
        }
        "sftp" => {
            let cfg: crate::storage::sftp::SftpConfig =
                serde_json::from_value(config.clone()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
            Ok(Arc::new(crate::storage::sftp::SftpStorage::new(&cfg)?))
        }
        "ftp" => {
            let cfg: crate::storage::ftp::FtpConfig =
                serde_json::from_value(config.clone()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
            Ok(Arc::new(crate::storage::ftp::FtpStorage::new(&cfg)?))
        }
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "Unsupported storage type")),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn insecure_connections_only_to_private_networks() {
        use serde_json::json;
        let ok = |kind: &'static str, cfg: serde_json::Value| async move { check_insecure_target(kind, &cfg).await };
        assert!(ok("s3", json!({ "endpoint": "http://127.0.0.1:9100", "allow_http": true })).await.is_ok());
        assert!(ok("s3", json!({ "endpoint": "http://192.168.1.20:9000/bucket", "allow_http": true })).await.is_ok());
        assert!(ok("s3", json!({ "endpoint": "http://[fd00::5]:9000", "allow_http": true })).await.is_ok());
        assert!(ok("s3", json!({ "endpoint": "http://8.8.8.8:9000", "allow_http": true })).await.is_err());
        assert!(ok("s3", json!({ "endpoint": "HTTP://8.8.8.8:9000", "allow_http": true })).await.is_err(), "case-insensitive");
        assert!(ok("s3", json!({ "endpoint": "http://user@8.8.8.8:9000", "allow_http": true })).await.is_err(), "credentials in the URL");
        assert!(ok("s3", json!({ "endpoint": "http://no-such-host.invalid:9000", "allow_http": true })).await.is_err(), "rejected when unresolvable");
        assert!(ok("s3", json!({ "endpoint": "https://8.8.8.8", "allow_http": true })).await.is_ok(), "https isn't restricted");
        assert!(ok("ftp", json!({ "host": "10.0.0.8", "tls": false })).await.is_ok());
        assert!(ok("ftp", json!({ "host": "100.101.102.103", "tls": true, "tls_insecure": true })).await.is_ok());
        let e = ok("ftp", json!({ "host": "1.1.1.1", "tls": false })).await.unwrap_err();
        assert!(e.contains("Unencrypted FTP") && e.contains("1.1.1.1"), "{e}");
        assert!(ok("ftp", json!({ "host": "1.1.1.1", "tls": true, "tls_insecure": true })).await.is_err());
        assert!(ok("ftp", json!({ "host": "1.1.1.1", "tls": true })).await.is_ok());
        assert!(ok("sftp", json!({ "host": "1.1.1.1" })).await.is_ok());
    }

    #[tokio::test]
    async fn sftp_and_ftp_hosts_are_split_into_parts() {
        let cfg = normalize("sftp", serde_json::json!({ "host": "sftp://backup@nas.local:2222/volume1/drive/", "username": "" })).await;
        assert_eq!((cfg["host"].as_str(), cfg["port"].as_u64(), cfg["path"].as_str(), cfg["username"].as_str()), (Some("nas.local"), Some(2222), Some("/volume1/drive"), Some("backup")));
        // Fields already filled in aren't overwritten; nothing changes without a port
        let cfg = normalize("ftp", serde_json::json!({ "host": " ftp.example.com ", "path": "/data", "port": 2121 })).await;
        assert_eq!((cfg["host"].as_str(), cfg["port"].as_u64(), cfg["path"].as_str()), (Some("ftp.example.com"), Some(2121), Some("/data")));
        let cfg = normalize("ftp", serde_json::json!({ "host": "[::1]:21" })).await;
        assert_eq!((cfg["host"].as_str(), cfg["port"].as_u64()), (Some("::1"), Some(21)));
    }

    #[tokio::test]
    async fn normalize_splits_bucket_from_endpoint() {
        let cfg = serde_json::json!({ "endpoint": "https://acct.r2.cloudflarestorage.com/files/", "region": "", "bucket": "" });
        let out: S3Config = serde_json::from_value(normalize("s3", cfg).await).unwrap();
        assert_eq!((out.endpoint.as_str(), out.bucket.as_str(), out.region.as_str()), ("https://acct.r2.cloudflarestorage.com", "files", "auto"));
    }
}
