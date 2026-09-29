FROM rust:1.97-alpine3.21 AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release --jobs 1 && \
    cp /app/target/release/grepcal /grepcal

FROM scratch

COPY --from=builder /grepcal /grepcal
COPY --from=builder /etc/ssl/cert.pem /etc/ssl/cert.pem

ENV SSL_CERT_FILE=/etc/ssl/cert.pem
USER 65532:65532
EXPOSE 8080
VOLUME ["/config"]
ENTRYPOINT ["/grepcal", "/config/config.toml"]
