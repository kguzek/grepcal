use std::{
    env, fs,
    io::{BufReader, Cursor},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
    routing::get,
    Router,
};
use bytes::Bytes;
use ical::IcalParser;
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::{sync::RwLock, time};
use tower_http::trace::TraceLayer;
use tracing::{error, info};

#[derive(Deserialize)]
struct Config {
    source_url: String,
    listen_address: SocketAddr,
    refresh_seconds: u64,
    cache_max_age_seconds: u64,
    #[serde(default)]
    filters: Vec<FilterConfig>,
}

#[derive(Deserialize)]
struct FilterConfig {
    pattern: String,
    action: FilterAction,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum FilterAction {
    Include,
    Exclude,
}

struct Filter {
    pattern: Regex,
    action: FilterAction,
}

struct Snapshot {
    body: Bytes,
    etag: HeaderValue,
}

struct AppState {
    snapshot: RwLock<Snapshot>,
    cache_control: HeaderValue,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config_path = env::args()
        .nth(1)
        .unwrap_or_else(|| "config.toml".to_owned());
    let config: Config = toml::from_str(
        &fs::read_to_string(&config_path)
            .with_context(|| format!("could not read configuration from {config_path}"))?,
    )
    .with_context(|| format!("could not parse configuration from {config_path}"))?;
    let filters = compile_filters(config.filters)?;
    let client = Client::builder()
        .user_agent("grepcal/0.1")
        .timeout(Duration::from_secs(30))
        .build()?;

    let initial = fetch_snapshot(&client, &config.source_url, &filters).await?;
    let state = Arc::new(AppState {
        snapshot: RwLock::new(initial),
        cache_control: HeaderValue::from_str(&format!(
            "public, max-age={}",
            config.cache_max_age_seconds
        ))?,
    });

    tokio::spawn(refresh_loop(
        Arc::clone(&state),
        client,
        config.source_url,
        filters,
        Duration::from_secs(config.refresh_seconds),
    ));

    let app = Router::new()
        .route("/calendar.ics", get(calendar))
        .route("/health", get(health))
        .with_state(state)
        .layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(config.listen_address).await?;
    info!(address = %config.listen_address, "serving filtered calendar");
    axum::serve(listener, app).await?;
    Ok(())
}

fn compile_filters(configs: Vec<FilterConfig>) -> Result<Vec<Filter>> {
    configs
        .into_iter()
        .map(|config| {
            Ok(Filter {
                pattern: Regex::new(&config.pattern)
                    .with_context(|| format!("invalid filter pattern: {}", config.pattern))?,
                action: config.action,
            })
        })
        .collect()
}

async fn refresh_loop(
    state: Arc<AppState>,
    client: Client,
    source_url: String,
    filters: Vec<Filter>,
    interval: Duration,
) {
    let mut ticker = time::interval(interval);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        match fetch_snapshot(&client, &source_url, &filters).await {
            Ok(snapshot) => {
                *state.snapshot.write().await = snapshot;
                info!("refreshed calendar snapshot");
            }
            Err(error) => {
                error!(%error, "calendar refresh failed; continuing to serve the last snapshot")
            }
        }
    }
}

async fn fetch_snapshot(client: &Client, source_url: &str, filters: &[Filter]) -> Result<Snapshot> {
    let source = client
        .get(source_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let body = filter_calendar(&source, filters)?;
    let etag = format!("\"{:x}\"", Sha256::digest(&body));
    Ok(Snapshot {
        body: Bytes::from(body),
        etag: HeaderValue::from_str(&etag)?,
    })
}

async fn calendar(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response<Body> {
    let snapshot = state.snapshot.read().await;
    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|value| value == snapshot.etag);
    let status = if not_modified {
        StatusCode::NOT_MODIFIED
    } else {
        StatusCode::OK
    };
    let body = if not_modified {
        Body::empty()
    } else {
        Body::from(snapshot.body.clone())
    };

    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/calendar; charset=utf-8")
        .header(header::ETAG, &snapshot.etag)
        .header(header::CACHE_CONTROL, &state.cache_control)
        .body(body)
        .expect("static response headers are valid")
}

async fn health() -> StatusCode {
    StatusCode::NO_CONTENT
}

fn filter_calendar(source: &[u8], filters: &[Filter]) -> Result<Vec<u8>> {
    let parsed = IcalParser::new(BufReader::new(Cursor::new(source)))
        .next()
        .context("calendar is empty")??;
    if parsed.events.is_empty() {
        return Ok(source.to_vec());
    }

    let keep: Vec<bool> = parsed
        .events
        .iter()
        .map(|event| {
            let summary = event
                .properties
                .iter()
                .find(|property| property.name.eq_ignore_ascii_case("SUMMARY"))
                .and_then(|property| property.value.as_deref());
            should_keep(summary, filters)
        })
        .collect();

    let spans = event_spans(source)?;
    if spans.len() != keep.len() {
        bail!("parsed event count does not match source event count");
    }

    let mut result = Vec::with_capacity(source.len());
    let mut start = 0;
    for ((event_start, event_end), keep) in spans.into_iter().zip(keep) {
        result.extend_from_slice(&source[start..event_start]);
        if keep {
            result.extend_from_slice(&source[event_start..event_end]);
        }
        start = event_end;
    }
    result.extend_from_slice(&source[start..]);
    Ok(result)
}

fn should_keep(summary: Option<&str>, filters: &[Filter]) -> bool {
    let Some(summary) = summary else {
        return true;
    };
    filters.iter().fold(true, |keep, filter| {
        if filter.pattern.is_match(summary) {
            matches!(filter.action, FilterAction::Include)
        } else {
            keep
        }
    })
}

fn event_spans(source: &[u8]) -> Result<Vec<(usize, usize)>> {
    let text = std::str::from_utf8(source).context("calendar is not valid UTF-8")?;
    let mut spans = Vec::new();
    let mut event_start = None;
    let mut depth = 0_u32;
    let mut offset = 0;

    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if content.eq_ignore_ascii_case("BEGIN:VEVENT") {
            if event_start.is_some() {
                bail!("nested VEVENT component");
            }
            event_start = Some(offset);
            depth = 1;
        } else if event_start.is_some() && content.starts_with("BEGIN:") {
            depth += 1;
        } else if event_start.is_some() && content.starts_with("END:") {
            depth = depth.checked_sub(1).context("invalid component nesting")?;
            if depth == 0 {
                spans.push((
                    event_start.take().expect("event start is set"),
                    offset + line.len(),
                ));
            }
        }
        offset += line.len();
    }
    if event_start.is_some() {
        bail!("unterminated VEVENT component");
    }
    Ok(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_matching_event_without_changing_other_components() {
        let source = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:UTC\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nSUMMARY:Keep me\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nSUMMARY:Skip me\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let filters = vec![Filter {
            pattern: Regex::new("Skip").unwrap(),
            action: FilterAction::Exclude,
        }];

        let result = filter_calendar(source, &filters).unwrap();
        let result = String::from_utf8(result).unwrap();
        assert!(result.contains("SUMMARY:Keep me"));
        assert!(!result.contains("SUMMARY:Skip me"));
        assert!(result.contains("BEGIN:VTIMEZONE"));
    }
}
