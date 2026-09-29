FROM rust:1.97-alpine3.21 AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --release --jobs 1

FROM scratch

COPY --from=builder /app/target/release/grepcal /grepcal
COPY --from=builder /etc/ssl/cert.pem /etc/ssl/cert.pem

ENV SSL_CERT_FILE=/etc/ssl/cert.pem
USER 65532:65532
EXPOSE 8080
VOLUME ["/config"]
ENTRYPOINT ["/grepcal", "/config/config.toml"]
