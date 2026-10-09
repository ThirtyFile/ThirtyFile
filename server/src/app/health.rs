//! The health check: `GET /api/health`, and the `thirtyfile health` probe that asks for it.

use std::time::Duration;

use crate::{state::AppState, util};

/// `thirtyfile health`: connects to the local listen address (0.0.0.0 becomes 127.0.0.1) and requests /api/health
pub async fn health_probe(addr: &str) -> bool {
    // A host name (localhost:8080) may resolve to several addresses (IPv6, IPv4): try each one
    let targets: Vec<std::net::SocketAddr> = match addr.parse::<std::net::SocketAddr>() {
        Ok(a) => vec![a],
        Err(_) => tokio::net::lookup_host(addr).await.map(|list| list.collect()).unwrap_or_default(),
    };
    if targets.is_empty() {
        eprintln!("Invalid listen address: {addr}");
        return false;
    }
    for mut target in targets {
        if target.ip().is_unspecified() {
            target.set_ip(if target.is_ipv4() { std::net::Ipv4Addr::LOCALHOST.into() } else { std::net::Ipv6Addr::LOCALHOST.into() });
        }
        if probe_once(target).await {
            return true;
        }
    }
    false
}

async fn probe_once(target: std::net::SocketAddr) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let probe = async {
        let mut stream = tokio::net::TcpStream::connect(target).await?;
        stream.write_all(b"GET /api/health HTTP/1.0\r\nHost: localhost\r\n\r\n").await?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;
        Ok::<_, std::io::Error>(String::from_utf8_lossy(&buf).into_owned())
    };
    match tokio::time::timeout(Duration::from_secs(5), probe).await {
        Ok(Ok(res)) => {
            let status = res.lines().next().unwrap_or_default().to_string();
            let body = res.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or_default();
            println!("{status} {body}");
            status.split_whitespace().nth(1) == Some("200")
        }
        Ok(Err(e)) => {
            eprintln!("Can't connect to {target}: {e}");
            false
        }
        Err(_) => {
            eprintln!("Connection to {target} timed out");
            false
        }
    }
}

/// Free space below this (on /data or /storage) makes the health check report "degraded": a full disk is the most likely
/// outage of a file server, and SQLite fails when it can't write
const LOW_DISK: u64 = 1024 * 1024 * 1024;

/// Health check (for Docker HEALTHCHECK and load balancers): no sign-in required; checks that the database is readable.
/// It doesn't name the version, which would tell anyone which release to look up for known problems: people who are
/// signed in get it from /api/auth/me.
pub async fn health(axum::extract::State(st): axum::extract::State<AppState>) -> axum::response::Response {
    use axum::response::IntoResponse;
    if let Err(e) = sqlx::query_scalar::<_, i64>("SELECT 1").fetch_one(&st.db).await {
        tracing::warn!("health check failed: {e}");
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, axum::Json(serde_json::json!({ "status": "error" }))).into_response();
    }
    let mut warnings = Vec::new();
    let mut disks = serde_json::Map::new();
    for (name, path) in [("data", &st.data_dir), ("storage", &st.storage_dir)] {
        // A disk that doesn't answer leaves its figures out, rather than the health check
        if let Some((free, total)) = util::disk_space_soon(path).await {
            if free < LOW_DISK {
                warnings.push(format!("{name}: {} free", util::format_bytes_u64(free)));
            }
            disks.insert(name.into(), serde_json::json!({ "free_bytes": free, "total_bytes": total }));
        }
    }
    // Only whether each storage location is reachable: the reasons can name servers, and this endpoint is public
    let locations: serde_json::Map<String, serde_json::Value> =
        st.location_health.lock().iter().map(|(id, h)| (id.clone(), serde_json::Value::from(if h.ok { "ok" } else { "offline" }))).collect();
    for (id, s) in &locations {
        if s != "ok" {
            warnings.push(format!("storage location {id} is offline"));
        }
    }
    // A background task that stopped on an error was started again: worth a look at the logs, for a day
    if util::last_restart().is_some_and(|t| util::now() - t < 86400) {
        warnings.push("a background task stopped on an error and was started again".into());
    }
    let status = if warnings.is_empty() { "ok" } else { "degraded" };
    if !warnings.is_empty() {
        // Docker asks every 30 s: warn at most once an hour
        static LAST: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let t = util::now();
        if t - LAST.load(std::sync::atomic::Ordering::Relaxed) >= 3600 {
            LAST.store(t, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!("Health check degraded: {}", warnings.join("; "));
        }
    }
    // Still 200: restarting the container doesn't free disk space or bring a storage service back
    axum::Json(serde_json::json!({ "status": status, "disks": disks, "locations": locations })).into_response()
}

#[cfg(test)]
mod tests {
    use axum::{http::StatusCode, response::Response};

    use super::*;
    use crate::state;

    #[tokio::test]
    async fn health_reports_disks_and_offline_storage_locations() {
        let env = crate::testutil::env().await;
        let body = |res: Response| async { serde_json::from_slice::<serde_json::Value>(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap() };
        let res = health(axum::extract::State(env.st.clone())).await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body(res).await;
        assert!(v.get("version").is_none(), "anyone can ask, so it doesn't name the release");
        // Disk space is only read on Unix
        assert!(!cfg!(unix) || v["disks"]["data"]["total_bytes"].as_u64().unwrap() > 0);
        env.st.location_health.lock().insert("nas".into(), state::LocationHealth { ok: false, error: Some("secret host".into()), checked_at: 0 });
        let v = body(health(axum::extract::State(env.st.clone())).await).await;
        assert_eq!((v["status"].as_str(), v["locations"]["nas"].as_str()), (Some("degraded"), Some("offline")));
        assert!(!v.to_string().contains("secret host"), "reasons stay private");
    }

    /// A server on a local port that answers every request with `reply`, once per connection
    async fn answering(reply: &'static str) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(reply.as_bytes()).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn the_probe_asks_the_local_server_and_succeeds_only_on_200() {
        // The real endpoint, served on a local port
        let env = crate::testutil::env().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = axum::Router::new().route("/api/health", axum::routing::get(health)).with_state(env.st.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        assert!(health_probe(&format!("127.0.0.1:{port}")).await);
        // Listening on every address: the probe asks this machine
        assert!(health_probe(&format!("0.0.0.0:{port}")).await);
        // A name tried address by address (localhost may be ::1 first, where nothing listens)
        assert!(health_probe(&format!("localhost:{port}")).await);

        let failing = answering("HTTP/1.1 503 Service Unavailable\r\nContent-Length: 18\r\n\r\n{\"status\":\"error\"}").await;
        assert!(!health_probe(&format!("127.0.0.1:{failing}")).await);
        let garbled = answering("hello").await;
        assert!(!health_probe(&format!("127.0.0.1:{garbled}")).await);
        // Nothing listening, and an address that isn't one
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
        assert!(!health_probe(&format!("127.0.0.1:{closed}")).await);
        assert!(!health_probe("not an address").await);
    }
}
