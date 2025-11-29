# Builder stage
# Use a Rust toolchain new enough for reqwest/axum transitive ICU crates (needs 1.83)
FROM rust:1.91-slim-trixie AS builder

WORKDIR /app

# openssl-sys 用にビルド依存をインストール
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*

# 依存関係をキャッシュさせるために先に Cargo.toml をコピー
COPY Cargo.toml .

# ビルド用のダミー src を一旦置く
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release || true

COPY src ./src

RUN cargo build --release

# Runtime stage
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/line-bot /usr/local/bin/line-bot

ENV RUST_LOG=info

EXPOSE 8080

CMD ["line-bot"]
