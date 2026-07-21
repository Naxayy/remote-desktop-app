# Build stage
FROM rust:1.75-slim AS builder
WORKDIR /build
COPY . .
RUN cargo build --release -p relay_server

# Runtime stage (imagen minima)
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/relay_server /usr/local/bin/relay_server
EXPOSE 3478/udp
CMD ["relay_server"]
