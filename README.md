# grepcal

> Like grep, but for iCal

Fetches one iCalendar feed, filters `VEVENT`s by `SUMMARY`, and serves the cached result at `/calendar.ics`.

## Local use

Copy `config.example.toml` to `config.toml` and change its filters, then run:

```sh
cargo run -- config.toml
```

The feed is available at `http://127.0.0.1:8080/calendar.ics`. There is also a health check available at `/health`.

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
