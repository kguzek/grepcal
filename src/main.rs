use std::{
    collections::HashMap,
    env, fs,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use axum::{http::HeaderValue, routing::get, Router};
use reqwest::Client;
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;
use tracing::info;

mod calendar;
mod config;
mod health;
mod landing;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config_path = env::args()
        .nth(1)
        .unwrap_or_else(|| "config.toml".to_owned());
    let config: config::Config = toml::from_str(
        &fs::read_to_string(&config_path)
            .with_context(|| format!("could not read configuration from {config_path}"))?,
    )
    .with_context(|| format!("could not parse configuration from {config_path}"))?;
    config::validate(&config)?;
    let client = Arc::new(
        Client::builder()
            .user_agent("grepcal/0.1")
            .timeout(Duration::from_secs(30))
            .build()?,
    );

    let mut calendars = HashMap::with_capacity(config.calendars.len());
    for calendar_config in config.calendars {
        let filters = calendar::compile_filters(calendar_config.filters)?;
        let calendar_state = Arc::new(calendar::CalendarState {
            snapshot: RwLock::new(None),
            cache_control: HeaderValue::from_str(&format!(
                "public, max-age={}",
                calendar_config.cache_max_age_seconds
            ))?,
            last_error: RwLock::new(Some("initial refresh has not completed".to_owned())),
        });
        calendars.insert(calendar_config.slug.clone(), Arc::clone(&calendar_state));
        tokio::spawn(calendar::refresh_loop(
            calendar_config.slug,
            calendar_state,
            Arc::clone(&client),
            calendar_config.source_url,
            filters,
            Duration::from_secs(calendar_config.refresh_seconds),
        ));
    }
    let state = Arc::new(health::AppState {
        calendars,
        started_at: Instant::now(),
    });

    let app = Router::new()
        .route("/", get(landing::home))
        .route("/{calendar}", get(calendar::serve))
        .route("/health", get(health::serve))
        .with_state(state)
        .layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(config.listen_address).await?;
    info!(address = %config.listen_address, "serving filtered calendars");
    axum::serve(listener, app).await?;
    Ok(())
}
