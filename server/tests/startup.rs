//! A server started the way the program starts it, on a new data folder

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
};
use clap::Parser;
use thirtyfile::{app::startup, cli::Config};
use tower::ServiceExt;

#[tokio::test]
async fn a_new_server_starts_lets_the_administrator_sign_in_and_keeps_its_data_folder_to_itself() {
    let dir = std::env::temp_dir().join(format!("thirtyfile-startup-{}", uuid::Uuid::new_v4()));
    let password = format!("pw-{}", uuid::Uuid::new_v4());
    let config = || Config::try_parse_from(["thirtyfile", "--data", dir.to_str().unwrap(), "--admin-password", &password]).unwrap();
    let start = || async {
        let cfg = config();
        let (db, _) = startup::open(&cfg.server).await?;
        startup::start(cfg.server, dir.join("blobs"), db).await
    };
    let server = start().await.unwrap();
    let app = server.router();

    let res = app.clone().oneshot(Request::get("/api/health").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let login = Request::post("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 5000))))
        .body(Body::from(serde_json::json!({ "username": "admin", "password": password }).to_string()))
        .unwrap();
    let res = app.clone().oneshot(login).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().contains_key(header::SET_COOKIE));

    // A second server on the same data folder doesn't start while this one runs
    let second = start().await.err().expect("the data folder is in use");
    assert_eq!(second.to_string(), "ThirtyFile is already running with this data folder. Stop it first.");

    server.state.db.close().await;
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}
