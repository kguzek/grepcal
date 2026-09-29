use std::{collections::HashMap, sync::Arc, time::Instant};

use axum::{extract::State, http::StatusCode, Json};
use chrono::{SecondsFormat, Utc};
use serde::Serialize;

use crate::calendar::CalendarState;

pub(crate) struct AppState {
    pub(crate) calendars: HashMap<String, Arc<CalendarState>>,
    pub(crate) started_at: Instant,
}

#[derive(Serialize)]
pub(crate) struct HealthResponse {
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

pub(crate) async fn serve(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<HealthResponse>) {
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

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc, time::Instant};

    use axum::{extract::State, http::HeaderValue, http::StatusCode, Json};
    use tokio::sync::RwLock;

    use super::{serve, AppState};
    use crate::calendar::CalendarState;

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
            .collect::<HashMap<_, _>>();
        Arc::new(AppState {
            calendars,
            started_at: Instant::now(),
        })
    }

    #[tokio::test]
    async fn reports_healthy_calendars() {
        let (status, Json(payload)) = serve(State(app_state(&[("work", None)]))).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload.status, "200 OK");
        assert!(payload.failed_calendars.is_empty());
        assert!(payload.uptime >= 0.0);
        assert!(chrono::DateTime::parse_from_rfc3339(&payload.date).is_ok());
    }

    #[tokio::test]
    async fn reports_failed_calendars_as_bad_gateway() {
        let (status, Json(payload)) = serve(State(app_state(&[
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
}
