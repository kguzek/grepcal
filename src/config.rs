use std::{collections::HashSet, net::SocketAddr};

use anyhow::{bail, Result};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) listen_address: SocketAddr,
    pub(crate) calendars: Vec<CalendarConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CalendarConfig {
    pub(crate) slug: String,
    pub(crate) source_url: String,
    pub(crate) refresh_seconds: u64,
    pub(crate) cache_max_age_seconds: u64,
    #[serde(default)]
    pub(crate) filters: Vec<FilterConfig>,
}

#[derive(Deserialize)]
pub(crate) struct FilterConfig {
    pub(crate) pattern: String,
    pub(crate) action: FilterAction,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FilterAction {
    Include,
    Exclude,
}

pub(crate) fn validate(config: &Config) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn calendar(slug: &str) -> CalendarConfig {
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
            calendars: vec![calendar("work"), calendar("work")],
        };
        assert!(validate(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate calendar slug"));

        let reserved = Config {
            listen_address: "127.0.0.1:8080".parse().unwrap(),
            calendars: vec![calendar("health")],
        };
        assert!(validate(&reserved)
            .unwrap_err()
            .to_string()
            .contains("invalid calendar slug"));
    }
}
