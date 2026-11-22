FROM rust:1.100.0-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
RUN cargo build --locked --release -p m53-server --features standalone
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/* && useradd -u 10001 -m app && mkdir /assets && chown app /assets
COPY --from=builder /src/target/release/m53-server /usr/local/bin/server
COPY public/deploy/entrypoint.sh /usr/local/bin/entrypoint
RUN chmod 755 /usr/local/bin/entrypoint
USER app
ENTRYPOINT ["/usr/local/bin/entrypoint"]
CMD ["serve"]
