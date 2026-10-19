//! Sending an alert to a webhook: a JSON body signed with HMAC-SHA256 so the receiver can tell it came from here, to an
//! address that isn't on the private network unless an administrator allowed that.

use std::net::SocketAddr;

use hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use sha2::Sha256;

use crate::storage::is_private_ip;

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// `sha256=<hex>`: HMAC-SHA256 of `<timestamp>.<body>` with the shared secret (the timestamp is sent as
/// `X-ThirtyFile-Timestamp`, so a receiver can refuse a message replayed later)
pub fn signature(secret: &str, timestamp: i64, body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// The address to send to: the URL checked, its host resolved, and the first address that may be connected to. Private
/// and loopback addresses (and carrier-grade NAT, as for other connections) only when `allow_private`; plain `http`
/// only to those, since nothing else should send an alert unencrypted.
pub async fn target(url: &str, allow_private: bool) -> Result<(reqwest::Url, Option<(String, SocketAddr)>), String> {
    let url = reqwest::Url::parse(url.trim()).map_err(|_| "the webhook address isn't a valid URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() {
        return Err("the webhook address must be http or https, without a user name or password in it".into());
    }
    let host = url.host_str().ok_or("the webhook address has no host")?.trim_matches(['[', ']']).to_string();
    let port = url.port_or_known_default().ok_or("the webhook address has no port")?;
    let (addrs, pin): (Vec<std::net::IpAddr>, bool) = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => (vec![ip], false),
        Err(_) => match tokio::time::timeout(std::time::Duration::from_secs(5), tokio::net::lookup_host((host.as_str(), port))).await {
            Ok(Ok(list)) => (list.map(|a| a.ip()).collect(), true),
            _ => return Err(format!("can't find the webhook's host {host}")),
        },
    };
    let allowed = |ip: &&std::net::IpAddr| allow_private || !is_private_ip(ip);
    let ip =
        *addrs.iter().find(allowed).ok_or_else(|| format!("{host} is on the private network (or loopback): allow that in the settings to send alerts there"))?;
    if url.scheme() == "http" && !is_private_ip(&ip) {
        return Err("a webhook on the internet must be https".into());
    }
    // Connected to the address checked, whatever the name resolves to later
    Ok((url, pin.then(|| (host, SocketAddr::new(ip, port)))))
}

/// Posts `payload` (signed with `secret`); the error is a short English reason, for the log, the page and the test
pub async fn post(url: &str, allow_private: bool, secret: &str, payload: &Value, timestamp: i64) -> Result<(), String> {
    let (url, pinned) = target(url, allow_private).await?;
    let body = serde_json::to_string(payload).map_err(|e| e.to_string())?;
    let mut client = reqwest::Client::builder().timeout(TIMEOUT).user_agent("ThirtyFile").redirect(reqwest::redirect::Policy::none());
    if let Some((host, addr)) = pinned {
        client = client.resolve(&host, addr);
    }
    let client = client.build().map_err(|e| e.to_string())?;
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("X-ThirtyFile-Timestamp", timestamp.to_string())
        .header("X-ThirtyFile-Signature", signature(secret, timestamp, &body))
        .body(body)
        .send()
        .await
        .map_err(|e| if e.is_timeout() { "the webhook didn't answer in time".to_string() } else { format!("can't reach the webhook ({})", e.without_url()) })?;
    match response.status() {
        s if s.is_success() => Ok(()),
        s => Err(format!("the webhook answered {s}")),
    }
}
