use std::{
    io::{BufReader, Cursor},
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
    response::IntoResponse,
};
use bytes::Bytes;
use ical::IcalParser;
use regex::Regex;
use reqwest::Client;
use sha2::{Digest, Sha256};
use tokio::{sync::RwLock, time};
use tracing::{error, info};

use crate::{
    config::{FilterAction, FilterConfig},
    health::AppState,
};

pub(crate) struct CalendarState {
    pub(crate) snapshot: RwLock<Option<Snapshot>>,
    pub(crate) cache_control: HeaderValue,
    pub(crate) last_error: RwLock<Option<String>>,
}

pub(crate) struct Snapshot {
    pub(crate) body: Bytes,
    pub(crate) etag: HeaderValue,
}

pub(crate) struct Filter {
    pattern: Regex,
    action: FilterAction,
}

pub(crate) fn compile_filters(configs: Vec<FilterConfig>) -> Result<Vec<Filter>> {
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

pub(crate) async fn refresh_loop(
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

pub(crate) async fn serve(
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
    use std::{collections::HashMap, sync::Arc, time::Instant};

    use axum::{
        extract::{Path, State},
        http::{HeaderMap, HeaderValue, StatusCode},
    };
    use bytes::Bytes;
    use regex::Regex;
    use tokio::sync::RwLock;

    use super::{filter_calendar, serve, CalendarState, Filter, Snapshot};
    use crate::{config::FilterAction, health::AppState};

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

    fn app_state() -> Arc<AppState> {
        let calendar = Arc::new(CalendarState {
            snapshot: RwLock::new(None),
            cache_control: HeaderValue::from_static("public, max-age=60"),
            last_error: RwLock::new(None),
        });
        Arc::new(AppState {
            calendars: HashMap::from([("work".to_owned(), calendar)]),
            started_at: Instant::now(),
        })
    }

    #[tokio::test]
    async fn requires_a_known_initialized_slug() {
        let state = app_state();
        let response = serve(
            Path("work.ics".to_owned()),
            State(Arc::clone(&state)),
            HeaderMap::new(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let response = serve(
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
        let response = serve(Path("work.ics".to_owned()), State(state), HeaderMap::new()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}
