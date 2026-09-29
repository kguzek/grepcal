use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::{BufReader, Cursor},
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use bytes::Bytes;
use chrono::{SecondsFormat, Utc};
use ical::IcalParser;
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{sync::RwLock, time};
use tower_http::trace::TraceLayer;
use tracing::{error, info};

mod landing;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    listen_address: SocketAddr,
    calendars: Vec<CalendarConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CalendarConfig {
    slug: String,
    source_url: String,
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

struct CalendarState {
    snapshot: RwLock<Option<Snapshot>>,
    cache_control: HeaderValue,
    last_error: RwLock<Option<String>>,
}

struct AppState {
    calendars: HashMap<String, Arc<CalendarState>>,
    started_at: Instant,
}

#[derive(Serialize)]
struct HealthResponse {
    message: &'static str,
    status: &'static str,
    uptime: f64,
    date: String,
    failed_calendars: Vec<FailedCalendar>,
}

#[derive(Serialize)]
struct FailedCalendar {
    slug: String,
    error: String,
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
    validate_config(&config)?;
    let client = Arc::new(
        Client::builder()
            .user_agent("grepcal/0.1")
            .timeout(Duration::from_secs(30))
            .build()?,
    );

    let mut calendars = HashMap::with_capacity(config.calendars.len());
    for calendar in config.calendars {
        let filters = compile_filters(calendar.filters)?;
        let calendar_state = Arc::new(CalendarState {
            snapshot: RwLock::new(None),
            cache_control: HeaderValue::from_str(&format!(
                "public, max-age={}",
                calendar.cache_max_age_seconds
            ))?,
            last_error: RwLock::new(Some("initial refresh has not completed".to_owned())),
        });
        calendars.insert(calendar.slug.clone(), Arc::clone(&calendar_state));
        tokio::spawn(refresh_loop(
            calendar.slug,
            calendar_state,
            Arc::clone(&client),
            calendar.source_url,
            filters,
            Duration::from_secs(calendar.refresh_seconds),
        ));
    }
    let state = Arc::new(AppState {
        calendars,
        started_at: Instant::now(),
    });

    let app = Router::new()
        .route("/", get(landing::home))
        .route("/{calendar}", get(calendar))
        .route("/health", get(health))
        .with_state(state)
        .layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(config.listen_address).await?;
    info!(address = %config.listen_address, "serving filtered calendars");
    axum::serve(listener, app).await?;
    Ok(())
}

fn validate_config(config: &Config) -> Result<()> {
    if config.calendars.is_empty() {
        bail!("configuration must contain at least one calendar");
    }

    let mut slugs = HashSet::with_capacity(config.calendars.len());
    for calendar in &config.calendars {
        let valid_slug = !calendar.slug.is_empty()
            && calendar.slug != "health"
            && calendar
                .slug
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if !valid_slug {
            bail!("invalid calendar slug: {}", calendar.slug);
        }
        if !slugs.insert(&calendar.slug) {
            bail!("duplicate calendar slug: {}", calendar.slug);
        }
        if calendar.refresh_seconds == 0 {
            bail!(
                "refresh_seconds must be greater than zero for {}",
                calendar.slug
            );
        }
    }
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
    slug: String,
    state: Arc<CalendarState>,
    client: Arc<Client>,
    source_url: String,
    filters: Vec<Filter>,
    interval: Duration,
) {
    loop {
        match fetch_snapshot(&client, &source_url, &filters).await {
            Ok(snapshot) => {
                *state.snapshot.write().await = Some(snapshot);
                *state.last_error.write().await = None;
                info!(slug, "refreshed calendar snapshot");
            }
            Err(refresh_error) => {
                let error_message = refresh_error.to_string();
                *state.last_error.write().await = Some(error_message.clone());
                error!(slug, error = %error_message, "calendar refresh failed; continuing to serve the last snapshot")
            }
        }
        time::sleep(interval).await;
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

async fn calendar(
    Path(path): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response<Body> {
    let Some(slug) = path.strip_suffix(".ics").filter(|slug| !slug.is_empty()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(state) = state.calendars.get(slug) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let snapshot = state.snapshot.read().await;
    let Some(snapshot) = snapshot.as_ref() else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
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

async fn health(State(state): State<Arc<AppState>>) -> (StatusCode, Json<HealthResponse>) {
    let mut failed_calendars = Vec::new();
    for (slug, calendar) in &state.calendars {
        if let Some(error) = calendar.last_error.read().await.clone() {
            failed_calendars.push(FailedCalendar {
                slug: slug.clone(),
                error,
            });
        }
    }
    failed_calendars.sort_by(|left, right| left.slug.cmp(&right.slug));

    let healthy = failed_calendars.is_empty();
    let status = if healthy {
        StatusCode::OK
    } else {
        StatusCode::BAD_GATEWAY
    };
    let status_text = if healthy { "200 OK" } else { "502 Bad Gateway" };
    (
        status,
        Json(HealthResponse {
            message: "Server is up",
            status: status_text,
            uptime: state.started_at.elapsed().as_secs_f64(),
            date: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            failed_calendars,
        }),
    )
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

    fn calendar_config(slug: &str) -> CalendarConfig {
        CalendarConfig {
            slug: slug.to_owned(),
            source_url: "https://calendar.example.com/feed.ics".to_owned(),
            refresh_seconds: 60,
            cache_max_age_seconds: 30,
            filters: Vec::new(),
        }
    }

    #[test]
    fn rejects_duplicate_and_reserved_calendar_slugs() {
        let duplicate = Config {
            listen_address: "127.0.0.1:8080".parse().unwrap(),
            calendars: vec![calendar_config("work"), calendar_config("work")],
        };
        assert!(validate_config(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate calendar slug"));

        let reserved = Config {
            listen_address: "127.0.0.1:8080".parse().unwrap(),
            calendars: vec![calendar_config("health")],
        };
        assert!(validate_config(&reserved)
            .unwrap_err()
            .to_string()
            .contains("invalid calendar slug"));
    }

    fn app_state(failures: &[(&str, Option<&str>)]) -> Arc<AppState> {
        let calendars = failures
            .iter()
            .map(|(slug, error)| {
                (
                    (*slug).to_owned(),
                    Arc::new(CalendarState {
                        snapshot: RwLock::new(None),
                        cache_control: HeaderValue::from_static("public, max-age=60"),
                        last_error: RwLock::new(error.map(str::to_owned)),
                    }),
                )
            })
            .collect();
        Arc::new(AppState {
            calendars,
            started_at: Instant::now(),
        })
    }

    #[tokio::test]
    async fn health_reports_healthy_calendars() {
        let (status, Json(payload)) = health(State(app_state(&[("work", None)]))).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload.status, "200 OK");
        assert!(payload.failed_calendars.is_empty());
        assert!(payload.uptime >= 0.0);
        assert!(chrono::DateTime::parse_from_rfc3339(&payload.date).is_ok());
    }

    #[tokio::test]
    async fn health_reports_failed_calendars_as_bad_gateway() {
        let (status, Json(payload)) = health(State(app_state(&[
            ("work", Some("upstream timed out")),
            ("personal", None),
        ])))
        .await;

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(payload.status, "502 Bad Gateway");
        assert_eq!(payload.failed_calendars.len(), 1);
        assert_eq!(payload.failed_calendars[0].slug, "work");
        assert_eq!(payload.failed_calendars[0].error, "upstream timed out");
    }

    #[tokio::test]
    async fn calendar_route_requires_a_known_initialized_slug() {
        let state = app_state(&[("work", None)]);
        let response = calendar(
            Path("work.ics".to_owned()),
            State(Arc::clone(&state)),
            HeaderMap::new(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let response = calendar(
            Path("missing.ics".to_owned()),
            State(Arc::clone(&state)),
            HeaderMap::new(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        *state.calendars["work"].snapshot.write().await = Some(Snapshot {
            body: Bytes::from_static(b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"),
            etag: HeaderValue::from_static("\"test\""),
        });
        let response = calendar(Path("work.ics".to_owned()), State(state), HeaderMap::new()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn home_includes_build_metadata() {
        let page = landing::home().await.0;

        assert!(page.contains("Like grep, but for iCal!"));
        assert!(page.contains("https://git.guzek.uk/kguzek/grepcal"));
        assert!(page.contains(env!("GREPCAL_REVISION")));
    }
}
