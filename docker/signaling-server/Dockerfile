# Build stage
FROM rust:1.75-slim AS builder
WORKDIR /build
COPY . .
RUN cargo build --release -p signaling_server

# Runtime stage (imagen minima)
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/signaling_server /usr/local/bin/signaling_server
EXPOSE 8080
CMD ["signaling_server"]
