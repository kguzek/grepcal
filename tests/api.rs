//! Black-box API tests: run the real binary against a local upstream feed.

use std::{
    fs,
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use axum::{http::StatusCode, routing::get, Router};
use reqwest::{header, Client, Response};

const CALENDAR: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nSUMMARY:Project review\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nSUMMARY:Daily standup\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

struct Server {
    process: Child,
    base_url: String,
    config_path: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = fs::remove_file(&self.config_path);
    }
}

async fn spawn_upstream() -> SocketAddr {
    let app = Router::new()
        .route("/calendar.ics", get(|| async { CALENDAR }))
        .route(
            "/broken.ics",
            get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

fn write_config(name: &str, contents: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("grepcal-{}-{name}.toml", std::process::id()));
    fs::write(&path, contents).unwrap();
    path
}

fn spawn_grepcal(name: &str, calendars: &str) -> Server {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config_path = write_config(
        name,
        &format!("listen_address = \"127.0.0.1:{port}\"\n{calendars}"),
    );
    let process = Command::new(env!("CARGO_BIN_EXE_grepcal"))
        .arg(&config_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Server {
        process,
        base_url: format!("http://127.0.0.1:{port}"),
        config_path,
    }
}

/// Polls `/health` until `ready` accepts the response or the deadline passes.
async fn wait_for_health(
    client: &Client,
    server: &Server,
    ready: impl Fn(StatusCode, &str) -> bool,
) -> (StatusCode, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(response) = client
            .get(format!("{}/health", server.base_url))
            .send()
            .await
        {
            let status = response.status();
            let body = response.text().await.unwrap();
            if ready(status, &body) {
                return (status, body);
            }
        }
        assert!(Instant::now() < deadline, "server did not become ready");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn get_path(client: &Client, server: &Server, path: &str) -> Response {
    client
        .get(format!("{}{path}", server.base_url))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn serves_filtered_calendars() {
    let upstream = spawn_upstream().await;
    let server = spawn_grepcal(
        "happy",
        &format!(
            "[[calendars]]\n\
             slug = \"work\"\n\
             source_url = \"http://{upstream}/calendar.ics\"\n\
             refresh_seconds = 300\n\
             cache_max_age_seconds = 60\n\
             [[calendars.filters]]\n\
             pattern = \"(?i)standup\"\n\
             action = \"exclude\"\n"
        ),
    );
    let client = Client::new();

    let (status, _) = wait_for_health(&client, &server, |status, _| status.is_success()).await;
    assert_eq!(status, StatusCode::OK);

    let landing = get_path(&client, &server, "/").await;
    assert_eq!(landing.status(), StatusCode::OK);
    assert!(landing.text().await.unwrap().contains("grepcal"));

    let calendar = get_path(&client, &server, "/work.ics").await;
    assert_eq!(calendar.status(), StatusCode::OK);
    assert_eq!(
        calendar.headers()[header::CONTENT_TYPE],
        "text/calendar; charset=utf-8"
    );
    assert_eq!(
        calendar.headers()[header::CACHE_CONTROL],
        "public, max-age=60"
    );
    let etag = calendar.headers()[header::ETAG].clone();
    let body = calendar.text().await.unwrap();
    assert!(body.contains("SUMMARY:Project review"));
    assert!(!body.contains("SUMMARY:Daily standup"));

    let revalidated = client
        .get(format!("{}/work.ics", server.base_url))
        .header(header::IF_NONE_MATCH, etag)
        .send()
        .await
        .unwrap();
    assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn reports_unavailable_sources_and_unknown_calendars() {
    let upstream = spawn_upstream().await;
    let server = spawn_grepcal(
        "sad",
        &format!(
            "[[calendars]]\n\
             slug = \"broken\"\n\
             source_url = \"http://{upstream}/broken.ics\"\n\
             refresh_seconds = 300\n\
             cache_max_age_seconds = 60\n"
        ),
    );
    let client = Client::new();

    let (status, body) = wait_for_health(&client, &server, |_, body| {
        body.contains("\"slug\":\"broken\"") && !body.contains("initial refresh")
    })
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("500 Internal Server Error"));

    let unavailable = get_path(&client, &server, "/broken.ics").await;
    assert_eq!(unavailable.status(), StatusCode::BAD_GATEWAY);

    for path in ["/missing.ics", "/broken", "/.ics"] {
        let response = get_path(&client, &server, path).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[test]
fn rejects_invalid_configuration() {
    let config_path = write_config(
        "invalid",
        "listen_address = \"127.0.0.1:0\"\ncalendars = []\n",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_grepcal"))
        .arg(&config_path)
        .output()
        .unwrap();
    fs::remove_file(&config_path).unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("at least one calendar"));
}
