# grepcal

> Like grep, but for iCal

Fetches multiple iCalendar feeds, filters `VEVENT`s by `SUMMARY`, and serves each cached result at `/{slug}.ics`.

## Local use

Copy `config.example.toml` to `config.toml` and change its filters, then run:

```sh
cargo run -- config.toml
```

Each `[[calendars]]` entry needs a unique `slug`, `source_url`, `refresh_seconds`, and `cache_max_age_seconds`; its `[[calendars.filters]]` rules apply only to that feed. For example, `slug = "my-cal"` is available at `http://127.0.0.1:8080/my-cal.ics`.

`/health` returns JSON with the current ISO 8601 UTC timestamp and floating-point uptime in seconds. It returns `200 OK` when every calendar is refreshing successfully, or `502 Bad Gateway` and a `failed_calendars` list when one or more sources are unavailable. The server still starts in the latter case and retries each source independently.

## Rules

Rules run in order and the last matching rule wins. Events with no `SUMMARY` remain in the output. `exclude` removes an event; `include` restores it after an earlier exclusion. Patterns are Rust regular expressions.

The service preserves the input bytes for all retained calendar components, including `VTIMEZONE` and `VALARM`, rather than reserializing them.

## Container

Build the static scratch image:

```sh
podman build -t grepcal .
```

Run it with a private configuration mounted read-only:

```sh
podman run --rm -p 8080:8080 -v "$PWD/config.toml:/config/config.toml:ro" grepcal
```
