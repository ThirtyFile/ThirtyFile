//! The accept loop, and stopping on a signal.

use std::time::Duration;

use axum::Router;
use tower::ServiceExt;

/// Ctrl+C or SIGTERM (sent by `docker stop` or when systemd stops the service): stop accepting new connections and wait for in-flight requests to finish
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
    tracing::info!("Received shutdown signal, shutting down…");
}

/// Accept loop with the limits `axum::serve` doesn't set: a connection that sends nothing, or doesn't finish sending
/// its request headers, within 30 seconds is dropped (so idle or slow connections can't pile up), and idle keep-alive
/// connections close too.
/// Shutdown waits for in-flight requests like `axum::serve` does.
pub async fn serve(listener: tokio::net::TcpListener, app: Router) -> Result<(), Box<dyn std::error::Error>> {
    use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
    let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    builder.http1().timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(30));
    builder.http2().timer(TokioTimer::new()).keep_alive_interval(Duration::from_secs(60)).keep_alive_timeout(Duration::from_secs(20));
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown_signal());
    loop {
        let (stream, addr) = tokio::select! {
            res = listener.accept() => match res {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            _ = &mut shutdown => break,
        };
        let app = app.clone();
        let service = hyper::service::service_fn(move |mut req: axum::http::Request<hyper::body::Incoming>| {
            req.extensions_mut().insert(axum::extract::ConnectInfo(addr));
            app.clone().oneshot(req)
        });
        let builder = builder.clone();
        let watcher = graceful.watcher();
        tokio::spawn(async move {
            // The protocol is detected from the first bytes, before hyper's own header timer starts: wait for them here
            let mut first = [0u8; 1];
            if !matches!(tokio::time::timeout(Duration::from_secs(30), stream.peek(&mut first)).await, Ok(Ok(n)) if n > 0) {
                return;
            }
            let conn = builder.serve_connection_with_upgrades(TokioIo::new(stream), service);
            let conn = watcher.watch(conn.into_owned());
            if let Err(e) = conn.await
                && !e.to_string().contains("connection closed")
            {
                tracing::debug!("Connection from {addr} ended with an error: {e}");
            }
        });
    }
    // Stop listening, so new connections are refused instead of waiting in the backlog
    drop(listener);
    // Running requests get a moment to finish; large transfers may be cut off. 20 s leaves time within Docker's stop
    // timeout (30 s in compose.yaml) to write the last log entries and close the database
    if tokio::time::timeout(Duration::from_secs(20), graceful.shutdown()).await.is_err() {
        tracing::warn!("Some connections were still open after 20 seconds and were closed");
    }
    Ok(())
}
